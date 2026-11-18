//! 数据库查询命令。
//!
//! 只做三件事：校验入参、把密文解出来、把活交给 `dbcli`。
//! 账号存在 `db_accounts`（密码是密文），查询走 SSH 通道在服务器上跑 mysql 客户端。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::connections::prepare_probe;
use crate::dbcli::schema::{self, SchemaSnapshot};
use crate::dbcli::{self, QueryResult};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::db_accounts::{self, DbAccount};

/// 默认 MySQL 端口。用户没改过就用这个。
const DEFAULT_PORT: u16 = 3306;

/// 给前端的账号视图：**不含密码**，只回答"配过没有、配的是谁"。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbAccountDto {
    pub configured: bool,
    pub username: String,
    pub port: u16,
    pub database: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbAccountInput {
    pub username: String,
    pub password: String,
    pub port: u16,
    #[serde(default)]
    pub database: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResultDto {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 输出超上限被截断 —— 前面的行仍然有效，界面要提示"还有更多"
    pub truncated: bool,
    pub elapsed_ms: u64,
}

impl DbAccountDto {
    fn empty() -> Self {
        Self {
            configured: false,
            username: String::new(),
            port: DEFAULT_PORT,
            database: String::new(),
        }
    }

    fn from_record(rec: &DbAccount) -> Self {
        Self {
            configured: true,
            username: rec.username.clone(),
            port: rec.port,
            database: rec.database_name.clone(),
        }
    }
}

#[tauri::command]
pub async fn db_account_get(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<DbAccountDto> {
    let rec = state
        .db
        .call(move |conn| db_accounts::get(conn, &connection_id))
        .await?;

    Ok(match rec {
        Some(rec) => DbAccountDto::from_record(&rec),
        None => DbAccountDto::empty(),
    })
}

#[tauri::command]
pub async fn db_account_save(
    state: State<'_, AppState>,
    connection_id: String,
    input: DbAccountInput,
) -> AppResult<DbAccountDto> {
    let username = input.username.trim().to_string();
    if username.is_empty() {
        return Err(AppError::validation("数据库用户名不能为空"));
    }
    // 密码允许留空：root 配成 auth_socket 时服务器上本来就没有密码，
    // 留空就让 mysql 走 socket 认证。填错了仍然会 1045，不会静默放行。
    if input.port == 0 {
        return Err(AppError::validation("端口要在 1–65535 之间"));
    }

    let cipher = state.require_cipher()?;
    let blob = cipher.encrypt_str(&input.password)?;

    let rec = DbAccount::new(connection_id.clone(), username, blob, input.port)
        .with_database(input.database.trim());
    let dto = DbAccountDto::from_record(&rec);

    state
        .db
        .call(move |conn| db_accounts::upsert(conn, &rec))
        .await?;

    // 换了账号，之前那条连接上验证过的身份就不能再用了
    state.dblinks.forget(&connection_id);

    Ok(dto)
}

#[tauri::command]
pub async fn db_account_clear(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<DbAccountDto> {
    state
        .db
        .call({
            let id = connection_id.clone();
            move |conn| db_accounts::delete_for_connection(conn, &id)
        })
        .await?;
    state.dblinks.forget(&connection_id);
    Ok(DbAccountDto::empty())
}

#[tauri::command]
pub async fn db_query(
    state: State<'_, AppState>,
    connection_id: String,
    sql: String,
    // 面板上临时选中的库。给 None 就用账号配置里的默认库。
    database: Option<String>,
) -> AppResult<QueryResultDto> {
    let sql = sql.trim().to_string();
    if sql.is_empty() {
        return Err(AppError::validation("SQL 不能为空"));
    }

    let (result, elapsed_ms) = run_sql(&state, &connection_id, database.as_deref(), &sql).await?;
    Ok(QueryResultDto {
        columns: result.columns,
        rows: result.rows,
        truncated: result.truncated,
        elapsed_ms,
    })
}

/// 库名列表，给面板顶部的下拉用。
///
/// **刻意不带默认库**：如果用户把默认库名写错了（`sgcc_opss` 之类），连上去会
/// 直接 `Unknown database`，连"列库名"都做不到，人就卡死在配置里出不来。
/// 列库名这件事本身不该依赖某个具体的库存不存在。
#[tauri::command]
pub async fn db_databases(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<Vec<String>> {
    let (result, _) = run_sql(&state, &connection_id, Some(""), "show databases").await?;
    // 只取第一列；`show databases` 永远只有一列叫 Database
    Ok(result
        .rows
        .into_iter()
        .filter_map(|row| row.first().cloned())
        .collect())
}

/// 当前默认库的表和列，给补全和表清单用。
///
/// 命中缓存时是纯内存操作，不会碰网络；`refresh = true` 强制重拉。
#[tauri::command]
pub async fn db_schema(
    state: State<'_, AppState>,
    connection_id: String,
    // 面板上临时选中的库；给 None 就用账号配置里的默认库。
    database: Option<String>,
    refresh: Option<bool>,
) -> AppResult<SchemaSnapshot> {
    let cipher = state.require_cipher()?;

    let rec = state
        .db
        .call({
            let id = connection_id.clone();
            move |conn| db_accounts::get(conn, &id)
        })
        .await?
        .ok_or_else(|| {
            AppError::validation("这台服务器还没配数据库账号，先在面板里填一次用户名和密码")
        })?;

    let effective = with_database(&rec, database.as_deref());

    if !refresh.unwrap_or(false) {
        if let Some(snapshot) = state
            .dblinks
            .cached_schema(&connection_id, &effective.database_name)
        {
            return Ok(snapshot);
        }
    }

    let password = cipher.decrypt_string(&rec.password_blob)?;
    let prepared = prepare_probe(&state, Some(connection_id.clone()), None, None).await?;
    let handle = state
        .dblinks
        .handle(&connection_id, &prepared.request)
        .await?;

    let snapshot = schema::fetch(&handle, &effective, &password).await?;
    state.dblinks.store_schema(&connection_id, snapshot.clone());
    Ok(snapshot)
}

/// 面板上选中的库优先；没选就用账号配置里的默认库。
///
/// `Some("")` 表示**明确不指定库** —— 列库名这类操作得能在默认库配错的情况下
/// 照常工作，否则用户会被锁在一个连不上的配置里，连改都没法改。
///
/// 只影响这一次请求，**不写回配置** —— 切库是浏览行为，不该悄悄改掉用户存的默认库。
fn with_database(rec: &DbAccount, database: Option<&str>) -> DbAccount {
    let mut effective = rec.clone();
    if let Some(db) = database {
        effective.database_name = db.trim().to_string();
    }
    effective
}

/// 取账号 → 解密 → 连上 → 执行。`db_query` 和 `db_databases` 共用。
async fn run_sql(
    state: &AppState,
    connection_id: &str,
    database: Option<&str>,
    sql: &str,
) -> AppResult<(QueryResult, u64)> {
    let cipher = state.require_cipher()?;

    let rec = state
        .db
        .call({
            let id = connection_id.to_string();
            move |conn| db_accounts::get(conn, &id)
        })
        .await?
        .ok_or_else(|| {
            AppError::validation("这台服务器还没配数据库账号，先在面板里填一次用户名和密码")
        })?;

    let effective = with_database(&rec, database);
    let password = cipher.decrypt_string(&rec.password_blob)?;

    let prepared = prepare_probe(state, Some(connection_id.to_string()), None, None).await?;
    let handle = state
        .dblinks
        .handle(connection_id, &prepared.request)
        .await?;

    let started = std::time::Instant::now();
    let result = dbcli::query(&handle, &effective, &password, sql).await?;
    Ok((result, started.elapsed().as_millis() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> DbAccount {
        DbAccount::new("c1", "root", b"blob".to_vec(), 3306).with_database("sgcc_ops")
    }

    #[test]
    fn selected_database_wins() {
        let effective = with_database(&account(), Some("taski"));
        assert_eq!(effective.database_name, "taski");
    }

    #[test]
    fn none_falls_back_to_the_configured_database() {
        let effective = with_database(&account(), None);
        assert_eq!(effective.database_name, "sgcc_ops");
    }

    /// 列库名时传空串。默认库写错的情况下必须能清掉，否则连"改回来"都做不到。
    #[test]
    fn empty_selection_clears_the_database() {
        assert!(with_database(&account(), Some("")).database_name.is_empty());
        assert!(with_database(&account(), Some("   "))
            .database_name
            .is_empty());
    }
}
