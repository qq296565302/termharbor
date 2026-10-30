//! 数据快照 command。
//!
//! 背景：0.1.1 的安装包升级时卸载器把整个应用数据目录删了，连接配置全丢。
//! 现在启动时自动留快照，并且只接受「列表里出现过的文件名」做恢复，
//! 不给前端传任意路径的口子。

use serde::Serialize;
use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::storage::backup;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupDto {
    /// 只暴露文件名，前端拿它做恢复标识
    pub name: String,
    pub created_ms: i64,
    pub bytes: u64,
    pub connections: Option<usize>,
}

#[tauri::command]
pub async fn backup_list(state: State<'_, AppState>) -> AppResult<Vec<BackupDto>> {
    let items = backup::list(&state.paths.backup_dir)?;
    Ok(items
        .into_iter()
        .map(|item| BackupDto {
            name: item.name,
            created_ms: item.created_ms,
            bytes: item.bytes,
            connections: item.connections,
        })
        .collect())
}

/// 立刻存一份快照。返回是否成功（库不存在时为 false，不算错误）。
#[tauri::command]
pub async fn backup_create(state: State<'_, AppState>) -> AppResult<bool> {
    let db_file = state.paths.db_file.clone();
    let backup_dir = state.paths.backup_dir.clone();

    // 先把 WAL 落盘，否则拷出来的库会缺最近写入
    state
        .db
        .call(|conn| {
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            Ok(())
        })
        .await?;

    let saved = backup::snapshot(&db_file, &backup_dir, backup::KEEP)?;
    if let Some(file) = &saved {
        tracing::info!(file = %file.display(), "手动快照已保存");
    }
    Ok(saved.is_some())
}

/// 安排恢复到某份快照。**下次启动生效**。
///
/// 不立刻覆盖：库连接还开着，直接换文件会让后续写入落到旧句柄上。
/// 前端拿到 true 后要提示用户重启。
#[tauri::command]
pub async fn backup_restore(state: State<'_, AppState>, name: String) -> AppResult<bool> {
    let file = backup::find(&state.paths.backup_dir, &name)?;
    backup::stage_restore(&file, &state.paths.db_file)?;
    tracing::warn!(snapshot = %name, "已安排恢复，等待重启生效");
    Ok(true)
}

/// 快照目录的绝对路径，给设置页显示（用户要自己备份/清理时能找到）。
#[tauri::command]
pub async fn backup_dir(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.paths.backup_dir.display().to_string())
}
