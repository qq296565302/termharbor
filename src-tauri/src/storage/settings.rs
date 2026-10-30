//! 设置项的读写。
//!
//! 表结构是 KV（`settings.key` / `settings.value`），所以这里只做最原始的两件事：
//! 全量读出、批量写入。**默认值与合法性校验不在这里** —— 那是 `commands::settings`
//! 的职责，因为它同时服务于「读出来要合并默认值」和「写进去要先 clamp」两个方向。

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::error::AppResult;

/// 读出所有键值。缺失的键由调用方用默认值补齐。
pub fn load_all(conn: &Connection) -> AppResult<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut out = HashMap::new();
    for row in rows {
        let (key, value) = row?;
        out.insert(key, value);
    }

    Ok(out)
}

/// 批量写入。已存在则覆盖。
///
/// 用一条 prepared statement 循环执行而不是拼接 SQL：设置项是全量提交的，
/// 但值来自前端，绝不能拼进 SQL 字符串。
pub fn upsert_many(conn: &Connection, pairs: &[(String, String)]) -> AppResult<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;

    for (key, value) in pairs {
        stmt.execute(params![key, value])?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations;

    fn memory_db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("内存库应当能打开");
        migrations::apply(&mut conn).expect("迁移应当成功");
        conn
    }

    #[test]
    fn empty_table_reads_as_empty_map() {
        let conn = memory_db();
        assert!(load_all(&conn).unwrap().is_empty());
    }

    #[test]
    fn upsert_overwrites_without_duplicating() {
        let conn = memory_db();

        upsert_many(
            &conn,
            &[("terminal.fontSize".to_string(), "14".to_string())],
        )
        .unwrap();
        upsert_many(
            &conn,
            &[("terminal.fontSize".to_string(), "16".to_string())],
        )
        .unwrap();

        let all = load_all(&conn).unwrap();
        assert_eq!(all.len(), 1, "同一个 key 不该出现两行");
        assert_eq!(all["terminal.fontSize"], "16");
    }

    #[test]
    fn multiple_keys_round_trip() {
        let conn = memory_db();
        upsert_many(
            &conn,
            &[
                ("terminal.theme".to_string(), "light".to_string()),
                ("terminal.scrollback".to_string(), "8000".to_string()),
            ],
        )
        .unwrap();

        let all = load_all(&conn).unwrap();
        assert_eq!(all["terminal.theme"], "light");
        assert_eq!(all["terminal.scrollback"], "8000");
    }
}
