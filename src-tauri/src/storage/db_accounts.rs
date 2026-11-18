//! 数据库账号：每台服务器一份，供终端页的数据库面板查询用。
//!
//! 和 `storage::connections` 一个口径：这一层只认识密文，加解密在命令层做；
//! 也不认识 SSH —— 账号只在"要在哪台机器上跑 mysql"这个语义下才有意义。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{AppError, AppResult, ErrCode};
use crate::storage::migrations::now_millis;

#[derive(Debug, Clone)]
pub struct DbAccount {
    pub connection_id: String,
    pub username: String,
    /// 密文（`[ver:1][nonce:12][ct][tag:16]`），二进制所以是 BLOB
    pub password_blob: Vec<u8>,
    pub port: u16,
    /// 默认库。空串表示"没指定"，面板里就让用户自己选。
    pub database_name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl DbAccount {
    pub fn new(
        connection_id: impl Into<String>,
        username: impl Into<String>,
        password_blob: impl Into<Vec<u8>>,
        port: u16,
    ) -> Self {
        let now = now_millis();
        Self {
            connection_id: connection_id.into(),
            username: username.into(),
            password_blob: password_blob.into(),
            port,
            database_name: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = database_name.into();
        self
    }
}

const COLUMNS: &str =
    "connection_id, username, password_blob, port, database_name, created_at, updated_at";

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<DbAccount> {
    Ok(DbAccount {
        connection_id: row.get(0)?,
        username: row.get(1)?,
        password_blob: row.get(2)?,
        port: row.get::<_, i64>(3)? as u16,
        database_name: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn get(conn: &Connection, connection_id: &str) -> AppResult<Option<DbAccount>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM db_accounts WHERE connection_id = ?1"),
            [connection_id],
            row_to_record,
        )
        .optional()?)
}

/// 有就覆盖、没有就插入。
///
/// 同一台服务器只留一份：改密码不该逼用户先删再建，也避免了"旧密码那条留着
/// 继续生效"这种更难理解的状态。
pub fn upsert(conn: &Connection, rec: &DbAccount) -> AppResult<()> {
    conn.execute(
        "INSERT INTO db_accounts
             (connection_id, username, password_blob, port, database_name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(connection_id) DO UPDATE SET
             username      = excluded.username,
             password_blob = excluded.password_blob,
             port          = excluded.port,
             database_name = excluded.database_name,
             updated_at    = excluded.updated_at",
        params![
            rec.connection_id,
            rec.username,
            rec.password_blob,
            rec.port as i64,
            rec.database_name,
            rec.created_at,
            rec.updated_at,
        ],
    )
    .map_err(|err| AppError::with_detail(ErrCode::StorageError, err.to_string()))?;
    Ok(())
}

/// 删连接时一起清掉它的数据库账号。
pub fn delete_for_connection(conn: &Connection, connection_id: &str) -> AppResult<()> {
    conn.execute(
        "DELETE FROM db_accounts WHERE connection_id = ?1",
        [connection_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    fn sample(connection_id: &str) -> DbAccount {
        DbAccount::new(connection_id, "root", b"blob".to_vec(), 13036)
    }

    #[tokio::test]
    async fn upsert_then_get_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let rec = sample("c1").with_database("sgcc_ops");

        db.call({
            let rec = rec.clone();
            move |conn| upsert(conn, &rec)
        })
        .await
        .unwrap();

        let stored = db.call(|conn| get(conn, "c1")).await.unwrap().unwrap();
        assert_eq!(stored.username, "root");
        assert_eq!(stored.password_blob, b"blob".to_vec());
        assert_eq!(stored.port, 13036);
        assert_eq!(stored.database_name, "sgcc_ops");
    }

    #[tokio::test]
    async fn upsert_overwrites_same_connection() {
        let db = Db::open_in_memory().unwrap();

        db.call(|conn| upsert(conn, &sample("c1"))).await.unwrap();

        let changed = DbAccount::new("c1", "ops", b"new-blob".to_vec(), 3306);
        db.call(move |conn| upsert(conn, &changed)).await.unwrap();

        let stored = db.call(|conn| get(conn, "c1")).await.unwrap().unwrap();
        assert_eq!(stored.username, "ops");
        assert_eq!(stored.password_blob, b"new-blob".to_vec());
        assert_eq!(stored.port, 3306);
    }

    #[tokio::test]
    async fn missing_connection_is_none_not_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.call(|conn| get(conn, "nope")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_for_connection_clears_only_that_one() {
        let db = Db::open_in_memory().unwrap();
        db.call(|conn| upsert(conn, &sample("c1"))).await.unwrap();
        db.call(|conn| upsert(conn, &sample("c2"))).await.unwrap();

        db.call(|conn| delete_for_connection(conn, "c1"))
            .await
            .unwrap();

        assert!(db.call(|conn| get(conn, "c1")).await.unwrap().is_none());
        assert!(db.call(|conn| get(conn, "c2")).await.unwrap().is_some());
    }
}
