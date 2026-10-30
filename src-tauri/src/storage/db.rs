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
        Self::open_with_snapshot(path, None)
    }

    /// 打开库，并在**跑迁移之前**留一份快照。
    ///
    /// 顺序有讲究：`wal_checkpoint(TRUNCATE)` 必须在拷文件之前做，
    /// 否则 WAL 里还没合并的数据会漏在快照外（表现为「恢复后少了几条最新配置」）。
    pub fn open_with_snapshot(
        path: impl AsRef<Path>,
        backup_dir: Option<&Path>,
    ) -> AppResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }

        // 必须在 Connection::open 之前判断：打开会把文件建出来，
        // 之后再判断永远是「存在」，首次启动也会拷一份空库出来。
        let existed = path.exists();

        let mut conn = Connection::open(&path)?;
        configure(&conn)?;

        if let Some(dir) = backup_dir {
            if !existed {
                // 首次启动：没有库可备份，跳过
            } else {
                // checkpoint 会返回 (busy, log, checkpointed) 三列，必须用 query_row
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
                match crate::storage::backup::snapshot(&path, dir, crate::storage::backup::KEEP) {
                    Ok(Some(file)) => tracing::info!(file = %file.display(), "已保存启动快照"),
                    Ok(None) => {}
                    Err(err) => {
                        // 备份失败不该拦住启动
                        tracing::warn!(error = %err, "启动快照失败，继续启动");
                    }
                }
            }
        }

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

    /// 首次启动不该产生空快照，第二次启动才该有。
    ///
    /// 这条守着一个真实的坑：`Connection::open` 会把文件建出来，
    /// 如果在打开之后才判断「库是否存在」，首次启动也会拷一份空库出来，
    /// 用户会在设置页看到一堆 0 台服务器的快照。
    #[test]
    fn snapshot_starts_from_the_second_launch() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("termharbor-db-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db_file = dir.join("termharbor.db");
        let backup_dir = dir.join("backups");

        // 第一次：库还不存在
        drop(Db::open_with_snapshot(&db_file, Some(&backup_dir)).unwrap());
        assert!(
            crate::storage::backup::list(&backup_dir)
                .unwrap()
                .is_empty(),
            "首次启动不该产生快照"
        );

        // 第二次：库已存在，应当留一份
        drop(Db::open_with_snapshot(&db_file, Some(&backup_dir)).unwrap());
        let items = crate::storage::backup::list(&backup_dir).unwrap();
        assert_eq!(items.len(), 1, "第二次启动应当留一份快照");
        assert_eq!(items[0].connections, Some(0), "新库里有 0 条连接");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_version_matches_latest() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::latest_version());
    }
}
