//! schema 迁移：`PRAGMA user_version` + 内嵌 SQL。
//!
//! 规则：
//! - 迁移只增不改。已发布的迁移文件不得再编辑，否则老库不会重跑；
//! - `version` 必须连续且在 `MIGRATIONS` 里升序；
//! - 每个迁移在独立事务里执行，失败即整体回滚。

use rusqlite::Connection;

use crate::error::AppResult;

pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/001_init.sql"),
    },
    Migration {
        version: 2,
        name: "connection_legacy_flag",
        sql: include_str!("../../migrations/002_connection_legacy_flag.sql"),
    },
];

/// 最新 schema 版本，供自检信息与前端版本校验使用。
pub fn latest_version() -> i32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn current_version(conn: &Connection) -> AppResult<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// 把库升级到最新版本。已是最新时是 no-op。
pub fn apply(conn: &mut Connection) -> AppResult<()> {
    let current = current_version(conn)?;
    // 先收集待应用项，避免在迭代中修改 current 造成借用冲突
    let pending: Vec<&Migration> = MIGRATIONS.iter().filter(|m| m.version > current).collect();

    for migration in pending {
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "应用数据库迁移"
        );

        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![migration.version, migration.name, now_millis()],
        )?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }

    Ok(())
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn
    }

    #[test]
    fn fresh_db_upgrades_to_latest() {
        let mut conn = memory_db();
        assert_eq!(current_version(&conn).unwrap(), 0);
        apply(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), latest_version());
    }

    #[test]
    fn apply_is_idempotent() {
        let mut conn = memory_db();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
    }

    #[test]
    fn expected_tables_exist() {
        let mut conn = memory_db();
        apply(&mut conn).unwrap();
        for table in [
            "connections",
            "groups",
            "known_hosts",
            "settings",
            "snippets",
            "meta",
        ] {
            let name: String = conn
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| panic!("缺少表 {table}"));
            assert_eq!(name, table);
        }
    }

    #[test]
    fn auth_type_check_constraint_is_enforced() {
        let mut conn = memory_db();
        apply(&mut conn).unwrap();
        let result = conn.execute(
            "INSERT INTO connections (id, name, host, port, username, auth_type, created_at, updated_at)
             VALUES ('1', 'n', 'h', 22, 'root', 'bogus', 0, 0)",
            [],
        );
        assert!(result.is_err(), "auth_type 应拒绝非法枚举值");
    }

    #[test]
    fn snippet_risk_level_constraint_is_enforced() {
        let mut conn = memory_db();
        apply(&mut conn).unwrap();
        let result = conn.execute(
            "INSERT INTO snippets (id, name, command, risk_level, created_at)
             VALUES ('1', 'n', 'ls', 'harmless', 0)",
            [],
        );
        assert!(result.is_err(), "risk_level 应拒绝未定义等级");
    }
}
