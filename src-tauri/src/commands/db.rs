//! 数据库查询命令。
//!
//! 只做三件事：校验入参、把密文解出来、把活交给 `dbcli`。
//! 账号存在 `db_accounts`（密码是密文），查询走 SSH 通道在服务器上跑 mysql 客户端。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::connections::prepare_probe;
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
    if input.password.is_empty() {
        return Err(AppError::validation("数据库密码不能为空"));
    }
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
) -> AppResult<QueryResultDto> {
    let sql = sql.trim().to_string();
    if sql.is_empty() {
        return Err(AppError::validation("SQL 不能为空"));
    }

    let (result, elapsed_ms) = run_sql(&state, &connection_id, &sql).await?;
    Ok(QueryResultDto {
        columns: result.columns,
        rows: result.rows,
        truncated: result.truncated,
        elapsed_ms,
    })
}

/// 库名列表，给面板顶部的下拉用。
#[tauri::command]
pub async fn db_databases(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<Vec<String>> {
    let (result, _) = run_sql(&state, &connection_id, "show databases").await?;
    // 只取第一列；`show databases` 永远只有一列叫 Database
    Ok(result
        .rows
        .into_iter()
        .filter_map(|row| row.first().cloned())
        .collect())
}

/// 取账号 → 解密 → 连上 → 执行。`db_query` 和 `db_databases` 共用。
async fn run_sql(
    state: &AppState,
    connection_id: &str,
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

    let password = cipher.decrypt_string(&rec.password_blob)?;

    let prepared = prepare_probe(state, Some(connection_id.to_string()), None, None).await?;
    let handle = state
        .dblinks
        .handle(connection_id, &prepared.request)
        .await?;

    let started = std::time::Instant::now();
    let result = dbcli::query(&handle, &rec, &password, sql).await?;
    Ok((result, started.elapsed().as_millis() as u64))
}
