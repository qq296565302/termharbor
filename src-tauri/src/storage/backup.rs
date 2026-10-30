//! 数据库快照与恢复。
//!
//! ## 为什么要有这个模块
//!
//! 0.1.1 的安装包升级时，NSIS 卸载器的「删除应用数据」把
//! `%APPDATA%\com.termharbor.app` 和 `%LOCALAPPDATA%\com.termharbor.app` 整个删掉了，
//! 用户的连接配置、known_hosts、日志一起没了。
//!
//! 那个删除动作只针对**以 bundle identifier 命名**的两个目录：
//!
//! ```nsis
//! RmDir /r "$APPDATA\${BUNDLEID}"
//! RmDir /r "$LOCALAPPDATA\${BUNDLEID}"
//! ```
//!
//! 所以快照放在同级的**产品名**目录 `TermHarbor\backups` 下 —— 不在这条删除路径上。
//! 这是唯一能扛住「用户勾了删除应用数据」的位置，代价是卸载后残留一个小目录，
//! 属于可接受的安全网（关于页会列出该路径，用户想清可以自己删）。
//!
//! ## 一次快照的时机
//!
//! 在 `Db::open` 里、**跑迁移之前**：先 `wal_checkpoint(TRUNCATE)` 把 WAL 落盘，
//! 再拷文件。没 checkpoint 就拷会缺掉最近写入（WAL 里还没合并进主库）。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::{AppError, AppResult};

/// 保留多少份历史快照。5 份足够覆盖「装错了 / 改坏了」的回退需求，
/// 又不会让备份目录无限长大（每份几十 KB 到几 MB）。
pub const KEEP: usize = 5;

/// 快照文件名前缀。目录里可能有别的东西，靠前缀区分。
const PREFIX: &str = "termharbor-";

/// 待恢复文件的固定名字。
///
/// 恢复不能直接覆盖正在使用的库（连接还开着），所以先落一个 pending 文件，
/// 下次启动、`Db::open` 之前再换上去。
const PENDING: &str = "restore.pending";

/// 一份快照的元信息，给前端列表用。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// 文件名（前端拿它做恢复的标识，避免暴露绝对路径）
    pub name: String,
    pub path: PathBuf,
    /// 快照生成时间（Unix 毫秒）
    pub created_ms: i64,
    pub bytes: u64,
    /// 里面有多少条连接。读不出来（老 schema / 文件损坏）就是 None
    pub connections: Option<usize>,
}

/// 备份目录：`<data_dir>/TermHarbor/backups`。
///
/// 传 `data_dir`（Windows 上是 `%APPDATA%`）而不是 app_data_dir，
/// 就是为了跳出 bundle id 目录 —— 见模块开头的说明。
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("TermHarbor").join("backups")
}

/// 拷一份当前库到备份目录，并按 `keep` 清理旧快照。
///
/// 库不存在时返回 `Ok(None)`（首次启动，没什么可备份的）。
pub fn snapshot(db_file: &Path, backup_dir: &Path, keep: usize) -> AppResult<Option<PathBuf>> {
    if !db_file.exists() {
        return Ok(None);
    }

    std::fs::create_dir_all(backup_dir)?;

    let name = format!("{PREFIX}{}.db", crate::state::now_millis());
    let target = backup_dir.join(&name);
    std::fs::copy(db_file, &target)?;

    prune(backup_dir, keep)?;
    Ok(Some(target))
}

/// 只保留最新的 `keep` 份快照。
///
/// 按文件名里的毫秒时间戳排序，而不是文件 mtime —— 复制、同步工具会改 mtime，
/// 文件名是我们自己写的，更可信。
pub fn prune(backup_dir: &Path, keep: usize) -> AppResult<()> {
    let mut items = snapshots_only(backup_dir)?;

    if items.len() <= keep {
        return Ok(());
    }

    // 新的在前，留住前 keep 个，其余删掉
    items.sort_by_key(|item| std::cmp::Reverse(item.0));
    for (_, path) in items.into_iter().skip(keep) {
        // 删不掉不该让启动失败：留个 warn 就够了
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(file = %path.display(), error = %err, "清理旧快照失败");
        }
    }

    Ok(())
}

/// 列出所有快照，新的在前。
pub fn list(backup_dir: &Path) -> AppResult<Vec<Snapshot>> {
    let mut items = snapshots_only(backup_dir)?;
    items.sort_by_key(|item| std::cmp::Reverse(item.0));

    Ok(items
        .into_iter()
        .map(|(created_ms, path)| {
            let name = path
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);

            Snapshot {
                name,
                connections: count_connections(&path),
                path,
                created_ms: created_ms as i64,
                bytes,
            }
        })
        .collect())
}

/// 按文件名找一份快照。
///
/// 只接受 `list` 里出现过的名字，不接受任意路径 —— 否则就是一个
/// 「前端传路径、后端读文件」的越界面。
pub fn find(backup_dir: &Path, name: &str) -> AppResult<PathBuf> {
    snapshots_only(backup_dir)?
        .into_iter()
        .find(|(_, path)| path.file_name().map(|value| value == name).unwrap_or(false))
        .map(|(_, path)| path)
        .ok_or_else(|| AppError::validation(format!("找不到这份快照: {name}")))
}

/// 把选中的快照登记为「下次启动时生效」。
///
/// 不直接覆盖当前库：rusqlite 的连接还开着，换文件只会让写入落到旧句柄上，
/// 表现为「恢复了但没生效」或直接写坏。落 pending 文件最稳。
pub fn stage_restore(snapshot_file: &Path, db_file: &Path) -> AppResult<()> {
    let pending = pending_path(db_file);
    std::fs::copy(snapshot_file, &pending)?;
    Ok(())
}

/// 启动时调用：有 pending 就先把当前库快照一份，再换上去。
///
/// 返回被换上去的文件路径；没有 pending 时返回 `None`。
pub fn apply_pending_restore(db_file: &Path, backup_dir: &Path) -> AppResult<Option<PathBuf>> {
    let pending = pending_path(db_file);
    if !pending.exists() {
        return Ok(None);
    }

    // 先把当前库存一份：万一用户点错了恢复，还有退路
    if let Err(err) = snapshot(db_file, backup_dir, KEEP) {
        tracing::warn!(error = %err, "恢复前快照失败，继续恢复");
    }

    // WAL/SHM 是旧库的，留着会让新库读到旧页
    for suffix in ["-wal", "-shm"] {
        let stale = PathBuf::from(format!("{}{suffix}", db_file.display()));
        if stale.exists() {
            let _ = std::fs::remove_file(&stale);
        }
    }

    std::fs::rename(&pending, db_file)?;
    tracing::warn!(target = %db_file.display(), "已应用待恢复的快照");
    Ok(Some(db_file.to_path_buf()))
}

fn pending_path(db_file: &Path) -> PathBuf {
    db_file.with_file_name(PENDING)
}

/// 目录里所有快照，返回 `(时间戳, 路径)`。
fn snapshots_only(backup_dir: &Path) -> AppResult<Vec<(u64, PathBuf)>> {
    if !backup_dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let name = match path.file_name().and_then(|value| value.to_str()) {
            Some(value) => value,
            None => continue,
        };

        let Some(stamp) = name
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.strip_suffix(".db"))
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };

        out.push((stamp, path));
    }

    Ok(out)
}

/// 读快照里的连接数。只读打开，失败就当读不出来（不报错）。
fn count_connections(path: &Path) -> Option<usize> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT COUNT(*) FROM connections", [], |row| {
        row.get::<_, i64>(0)
    })
    .ok()
    .map(|value| value.max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录（和测试基础设施同一约定：不用时间戳后缀）。
    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "termharbor-backup-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// 造一个只有 connections 表的最小库，不用跑完整迁移。
    fn make_db(path: &Path, rows: usize) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE connections (id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE meta (k TEXT);",
        )
        .unwrap();
        for index in 0..rows {
            conn.execute(
                "INSERT INTO connections (id, name) VALUES (?1, ?2)",
                [format!("c{index}"), format!("主机{index}")],
            )
            .unwrap();
        }
    }

    #[test]
    fn snapshot_copies_the_database_and_reads_back_counts() {
        let dir = temp_dir("snap");
        let db = dir.join("termharbor.db");
        make_db(&db, 3);

        let backup_dir = dir.join("backups");
        let file = snapshot(&db, &backup_dir, KEEP)
            .unwrap()
            .expect("应当生成快照");
        assert!(file.exists());

        let items = list(&backup_dir).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].connections, Some(3), "快照里应该能数出 3 条连接");
        assert!(items[0].bytes > 0);
    }

    #[test]
    fn snapshot_of_missing_database_is_a_no_op() {
        let dir = temp_dir("none");
        let result = snapshot(&dir.join("nope.db"), &dir.join("backups"), KEEP).unwrap();
        assert!(result.is_none(), "库不存在时不该报错，也不该造出空快照");
    }

    #[test]
    fn prune_keeps_only_the_newest_entries() {
        let dir = temp_dir("prune");
        let backup_dir = dir.join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // 手写文件名：不用 sleep 等时间戳，直接控制序号
        for stamp in [1000u64, 2000, 3000, 4000, 5000, 6000] {
            std::fs::write(backup_dir.join(format!("{PREFIX}{stamp}.db")), b"x").unwrap();
        }

        prune(&backup_dir, 3).unwrap();

        let names: Vec<String> = list(&backup_dir)
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "termharbor-6000.db",
                "termharbor-5000.db",
                "termharbor-4000.db"
            ],
            "留最新的三份，按文件名时间戳排"
        );
    }

    #[test]
    fn prune_ignores_unrelated_files() {
        let dir = temp_dir("unrelated");
        let backup_dir = dir.join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("readme.txt"), b"x").unwrap();
        std::fs::write(backup_dir.join("termharbor.db"), b"x").unwrap(); // 少了时间戳

        prune(&backup_dir, 1).unwrap();

        assert!(
            backup_dir.join("readme.txt").exists(),
            "非快照文件不能被误删"
        );
        assert!(backup_dir.join("termharbor.db").exists());
    }

    #[test]
    fn find_only_accepts_names_from_the_list() {
        let dir = temp_dir("find");
        let backup_dir = dir.join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("termharbor-1234.db"), b"x").unwrap();
        std::fs::write(dir.join("outside.db"), b"x").unwrap();

        assert!(find(&backup_dir, "termharbor-1234.db").is_ok());
        assert!(
            find(&backup_dir, "../outside.db").is_err(),
            "不能靠路径穿越拿到目录外的文件"
        );
        assert!(find(&backup_dir, "不存在.db").is_err());
    }

    #[test]
    fn staged_restore_replaces_the_database_on_next_start() {
        let dir = temp_dir("restore");
        let db = dir.join("termharbor.db");
        let backup_dir = dir.join("backups");

        // 现状：只有 1 条连接
        make_db(&db, 1);
        let file = snapshot(&db, &backup_dir, KEEP).unwrap().unwrap();

        // 用户又加了几条，然后决定恢复到快照
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute("DELETE FROM connections", []).unwrap();
            for index in 0..5 {
                conn.execute(
                    "INSERT INTO connections (id, name) VALUES (?1, ?2)",
                    [format!("x{index}"), "临时".to_string()],
                )
                .unwrap();
            }
        }

        stage_restore(&file, &db).unwrap();
        assert!(
            crate::storage::backup::pending_path(&db).exists(),
            "恢复先落 pending，不直接覆盖正在用的库"
        );

        let applied = apply_pending_restore(&db, &backup_dir).unwrap();
        assert!(applied.is_some());

        let conn = Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM connections", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "应当回到快照里的那 1 条");

        // 恢复前的状态也要留一份，点错了能退回来
        let items = list(&backup_dir).unwrap();
        assert!(
            items.len() >= 2,
            "恢复前应自动快照当前库，实际 {}",
            items.len()
        );

        // pending 用完就没了，重复启动不该再换一次
        assert!(apply_pending_restore(&db, &backup_dir).unwrap().is_none());
    }

    #[test]
    fn restore_drops_stale_wal_files() {
        let dir = temp_dir("wal");
        let db = dir.join("termharbor.db");
        let backup_dir = dir.join("backups");

        make_db(&db, 2);
        let file = snapshot(&db, &backup_dir, KEEP).unwrap().unwrap();

        // 制造假的旧 WAL/SHM：不清掉的话新库会读到旧页
        std::fs::write(PathBuf::from(format!("{}-wal", db.display())), b"stale").unwrap();
        std::fs::write(PathBuf::from(format!("{}-shm", db.display())), b"stale").unwrap();

        stage_restore(&file, &db).unwrap();
        apply_pending_restore(&db, &backup_dir).unwrap();

        assert!(!PathBuf::from(format!("{}-wal", db.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", db.display())).exists());
    }
}
