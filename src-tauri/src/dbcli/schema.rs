//! 库表结构快照：补全和表清单的数据来源。
//!
//! 只拉**当前默认库**的表和列。跨库一次拉全看起来省事，但输出会直接顶到
//! `query` 的 1MB 上限，而且补全列表里塞进来一堆用不上的表反而更难选。

use russh::client::Handle;
use serde::Serialize;

use super::query;
use crate::error::AppResult;
use crate::ssh::client::ProbeHandler;
use crate::storage::db_accounts::DbAccount;

/// 两列平铺，一行一个列。
///
/// **不要改成 `group_concat` 一行一表**：它受 `group_concat_max_len`（默认
/// 1024 字节）限制，列多的表会被静默截断；而要放宽这个限制得先 `set session`，
/// 属于第二条语句，现有解析器只认第一个结果集，会串。
const SCHEMA_SQL: &str = "select table_name, column_name \
     from information_schema.columns \
     where table_schema = database() \
     order by table_name, ordinal_position";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSnapshot {
    /// 这份快照属于哪个库。空串表示连接还没配默认库。
    pub database: String,
    pub tables: Vec<TableSchema>,
    /// 输出被上限截断，列表可能不全
    pub truncated: bool,
}

pub(crate) async fn fetch(
    conn: &Handle<ProbeHandler>,
    account: &DbAccount,
    password: &str,
) -> AppResult<SchemaSnapshot> {
    if account.database_name.is_empty() {
        // `database()` 在没有默认库时返回 NULL，查出来是空的 —— 与其发一次
        // 注定空手的往返，不如直接给出"还没配库"的信号。
        return Ok(SchemaSnapshot::default());
    }

    let result = query(conn, account, password, SCHEMA_SQL).await?;
    let mut snapshot = from_rows(&result.rows);
    snapshot.database = account.database_name.clone();
    snapshot.truncated = result.truncated;
    Ok(snapshot)
}

/// 行已经是按表名排好序的，所以只需要看最后一个表是不是同一张。
fn from_rows(rows: &[Vec<String>]) -> SchemaSnapshot {
    let mut tables: Vec<TableSchema> = Vec::new();

    for row in rows {
        let (Some(table), Some(column)) = (row.first(), row.get(1)) else {
            continue;
        };
        match tables.last_mut() {
            Some(last) if last.name == *table => last.columns.push(column.clone()),
            _ => tables.push(TableSchema {
                name: table.clone(),
                columns: vec![column.clone()],
            }),
        }
    }

    SchemaSnapshot {
        database: String::new(),
        tables,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(table: &str, column: &str) -> Vec<String> {
        vec![table.to_string(), column.to_string()]
    }

    #[test]
    fn groups_columns_under_their_table() {
        let rows = vec![
            row("orders", "id"),
            row("orders", "amount"),
            row("users", "id"),
            row("users", "name"),
            row("users", "email"),
        ];
        let snapshot = from_rows(&rows);

        assert_eq!(snapshot.tables.len(), 2);
        assert_eq!(snapshot.tables[0].name, "orders");
        assert_eq!(snapshot.tables[0].columns, vec!["id", "amount"]);
        assert_eq!(snapshot.tables[1].columns, vec!["id", "name", "email"]);
    }

    #[test]
    fn empty_result_is_an_empty_snapshot() {
        let snapshot = from_rows(&[]);
        assert!(snapshot.tables.is_empty());
    }

    /// 某一列为空（视图、生成列偶尔会这样）不能把整行丢掉，
    /// 否则表会凭空少一列，补全里就查不到它了。
    #[test]
    fn row_without_column_still_keeps_the_table() {
        let rows = vec![vec!["views_only".to_string(), String::new()]];
        let snapshot = from_rows(&rows);
        assert_eq!(snapshot.tables.len(), 1);
        assert_eq!(snapshot.tables[0].columns, vec![""]);
    }
}
