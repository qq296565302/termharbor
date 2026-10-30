//! SQLite 句柄封装。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};
use crate::storage::migrations;

pub struct Db {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Db {
    /// 打开（必要时创建）配置库，并升级到最新 schema。
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        let mut conn = Connection::open(&path)?;
        configure(&conn)?;
        migrations::apply(&mut conn)?;

        tracing::info!(
            path = %path.display(),
            schema = migrations::latest_version(),
            "本地配置库已就绪"
        );

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> AppResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn inner(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }

    pub fn schema_version(&self) -> AppResult<i32> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AppError::internal("配置库锁已中毒"))?;
        migrations::current_version(&guard)
    }

    /// 在阻塞线程池里执行一次同步 DB 操作。
    ///
    /// rusqlite 是同步库，**禁止**直接在 async 上下文里调用（约束 A-05 的同一思路：
    /// 别让慢 IO 占住 Tokio 工作线程）。
    pub async fn call<F, T>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&Connection) -> AppResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| AppError::internal("配置库锁已中毒"))?;
            f(&guard)
        })
        .await
        .map_err(|e| AppError::internal(format!("配置库任务异常: {e}")))?
    }
}

fn configure(conn: &Connection) -> AppResult<()> {
    // journal_mode 会返回结果集，必须用 query_row 而不是 pragma_update
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(mode = %mode, "未能启用 WAL 日志模式");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn call_runs_off_the_async_thread() {
        let db = Db::open_in_memory().unwrap();
        let version = db
            .call(|conn| Ok(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))?))
            .await
            .unwrap();
        assert_eq!(version, migrations::latest_version());
    }

    #[tokio::test]
    async fn call_propagates_errors() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .call(|conn| {
                conn.execute("INSERT INTO nope (a) VALUES (1)", [])?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrCode::StorageError);
    }

    #[test]
    fn schema_version_matches_latest() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::latest_version());
    }
}
