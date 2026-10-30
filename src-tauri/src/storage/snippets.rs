//! 自定义片段的读写（`snippets` 表里 `is_builtin = 0` 的那些）。
//!
//! 这一层只搬数据，不校验也不判风险：参数校验与 `classify_risk` 都在
//! `commands/snippets.rs`，和连接配置的分层保持一致。
//!
//! 表结构在 `001_init.sql` 里就有了，包含 `grp` / `risk_level` / `is_builtin` /
//! `builtin_key` / `sort` / `variables`，所以这里不需要新迁移。

use rusqlite::{params, Connection};

use crate::error::AppResult;
use crate::snippets::CustomSnippet;

/// 读出全部自定义片段，按 `sort` 再按创建时间排。
pub fn load_all(conn: &Connection) -> AppResult<Vec<CustomSnippet>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, command, COALESCE(description, ''), COALESCE(grp, '') \
         FROM snippets WHERE is_builtin = 0 ORDER BY sort, created_at",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(CustomSnippet {
            id: row.get(0)?,
            name: row.get(1)?,
            command: row.get(2)?,
            description: row.get(3)?,
            group: row.get(4)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 插入或更新一条自定义片段。
///
/// `risk_level` 有 NOT NULL + CHECK 约束，所以必须写；但读出来时不用它 ——
/// 展示和判定都以 `classify_risk` 现算的结果为准，避免有人手改库把危险命令标成只读。
pub fn upsert(
    conn: &Connection,
    item: &CustomSnippet,
    risk: &str,
    created_at: i64,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO snippets \
           (id, name, command, description, grp, risk_level, is_builtin, sort, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7) \
         ON CONFLICT(id) DO UPDATE SET \
           name = excluded.name, \
           command = excluded.command, \
           description = excluded.description, \
           grp = excluded.grp, \
           risk_level = excluded.risk_level",
        params![
            item.id,
            item.name,
            item.command,
            item.description,
            item.group,
            risk,
            created_at
        ],
    )?;

    Ok(())
}

/// 删除一条自定义片段。返回 false 表示不存在（或那条是内置的）。
///
/// `is_builtin = 0` 这个条件不是装饰：内置片段虽然存在代码里，
/// 但万一将来也 seed 进库，这条 SQL 能挡住误删。
pub fn delete(conn: &Connection, id: &str) -> AppResult<bool> {
    let affected = conn.execute(
        "DELETE FROM snippets WHERE id = ?1 AND is_builtin = 0",
        params![id],
    )?;
    Ok(affected > 0)
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

    fn sample(id: &str) -> CustomSnippet {
        CustomSnippet {
            id: id.to_string(),
            name: "部署 web".to_string(),
            command: "cd /opt/web && git pull && npm ci && npm run build".to_string(),
            description: "拉代码、装依赖、构建".to_string(),
            group: "web 项目".to_string(),
        }
    }

    #[test]
    fn empty_table_reads_as_empty_list() {
        let conn = memory_db();
        assert!(load_all(&conn).unwrap().is_empty());
    }

    #[test]
    fn upsert_then_load_round_trips() {
        let conn = memory_db();
        let item = sample("s1");

        upsert(&conn, &item, "dangerous", 1_700_000_000).unwrap();

        let all = load_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], item);
    }

    #[test]
    fn upsert_updates_in_place_without_duplicating() {
        let conn = memory_db();
        upsert(&conn, &sample("s1"), "dangerous", 1_700_000_000).unwrap();

        let mut edited = sample("s1");
        edited.command = "cd /opt/web && git pull".to_string();
        upsert(&conn, &edited, "dangerous", 1_700_000_001).unwrap();

        let all = load_all(&conn).unwrap();
        assert_eq!(all.len(), 1, "同一个 id 不该出现两行");
        assert_eq!(all[0].command, "cd /opt/web && git pull");
    }

    #[test]
    fn delete_removes_only_custom_snippets() {
        let conn = memory_db();
        upsert(&conn, &sample("s1"), "dangerous", 1).unwrap();

        // 手插一条"内置"记录，验证删不掉
        conn.execute(
            "INSERT INTO snippets (id, name, command, grp, risk_level, is_builtin, created_at) \
             VALUES ('builtin-1', '内置', 'df -h', '资源', 'readonly', 1, 1)",
            [],
        )
        .unwrap();

        assert!(delete(&conn, "s1").unwrap());
        assert!(!delete(&conn, "builtin-1").unwrap(), "内置片段删不掉");
        assert!(!delete(&conn, "不存在").unwrap());

        let all = load_all(&conn).unwrap();
        assert!(all.is_empty(), "自定义的已删，内置的不进这个列表");
    }

    #[test]
    fn list_is_sorted_by_sort_then_created_at() {
        let conn = memory_db();
        upsert(&conn, &sample("s2"), "dangerous", 200).unwrap();
        upsert(&conn, &sample("s1"), "dangerous", 100).unwrap();

        let all = load_all(&conn).unwrap();
        assert_eq!(all[0].id, "s1", "created_at 早的排前面");
    }
}
