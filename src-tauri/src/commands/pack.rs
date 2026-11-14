//! 打包传输 command：参数整理 + 后台任务，进度/取消/事件完全复用 `sftp_upload` 那套骨架。
//!
//! 这里故意不写业务逻辑：本地打包、远端拼命令、解包全在 `sftp::pack`。
//! 与 `sftp_upload` 一致，command 立刻返回 `transferId`，真正的活儿在后台跑，
//! 进度通过 `sftp:progress` 推送，前端传输队列无需为新功能改结构。

use std::path::PathBuf;

use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::commands::sftp::progress_emitter;
use crate::error::AppResult;
use crate::sftp::pack as engine;
use crate::state::AppState;

/// 把多个本地文件/目录打成一个 tar.gz 传到远端并解包，立即返回 `transferId`。
#[tauri::command]
pub async fn pack_upload(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_paths: Vec<String>,
    remote_dir: String,
) -> AppResult<String> {
    let conn = state.sftp.get(&session_id)?;
    let sources: Vec<PathBuf> = local_paths.into_iter().map(PathBuf::from).collect();

    // 和 sftp_upload 一样：必然失败的输入（空列表、路径不存在、应用数据目录当源）
    // 在给 transferId 之前就拦掉，免得进度条闪一串红条。
    let protected: [&std::path::Path; 2] = [&state.paths.data_dir, &state.paths.backup_dir];
    engine::validate_sources(&sources, &protected)?;

    let data_dir = state.paths.data_dir.clone();
    let backup_dir = state.paths.backup_dir.clone();
    // 传输 id 带会话前缀，cancel_session 靠它批量取消（与 sftp.rs 的 new_transfer_id 同构）
    let transfer_id = format!("{}::{}", session_id, state.next_session_seq());
    let token = CancellationToken::new();
    state.transfers.register(transfer_id.clone(), token.clone());

    let transfers = state.transfers.clone();
    let emit = progress_emitter(app, session_id.clone());
    let tid = transfer_id.clone();

    // 应用自己的数据目录：库里是加密凭据、快照里是配置，
    // 一律不许当上传源 —— local_paths 是 IPC 参数，不能假设来自文件对话框。
    tauri::async_runtime::spawn(async move {
        let protected: Vec<PathBuf> = vec![data_dir, backup_dir];
        let result = engine::upload(
            &conn,
            sources,
            remote_dir,
            protected,
            tid.clone(),
            token,
            emit,
        )
        .await;
        transfers.finish(&tid);

        match result {
            Ok(bytes) => tracing::info!(transfer = %tid, bytes, "打包上传完成"),
            Err(err) => tracing::warn!(
                transfer = %tid,
                code = err.code.as_str(),
                "打包上传失败: {}",
                err.message
            ),
        }
    });

    Ok(transfer_id)
}

/// 让远端把多个文件/目录打成一个 tar.gz，下到本机解包，立即返回 `transferId`。
#[tauri::command]
pub async fn pack_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    remote_paths: Vec<String>,
    local_dir: String,
) -> AppResult<String> {
    let conn = state.sftp.get(&session_id)?;

    // 同步前置校验：空列表 / 空目标目录没必要起后台任务。
    engine::validate_download_request(&remote_paths, &local_dir)?;

    let local = PathBuf::from(&local_dir);
    let transfer_id = format!("{}::{}", session_id, state.next_session_seq());
    let token = CancellationToken::new();
    state.transfers.register(transfer_id.clone(), token.clone());

    let transfers = state.transfers.clone();
    let emit = progress_emitter(app, session_id.clone());
    let tid = transfer_id.clone();

    tauri::async_runtime::spawn(async move {
        let result = engine::download(&conn, remote_paths, local, tid.clone(), token, emit).await;
        transfers.finish(&tid);

        match result {
            Ok(bytes) => tracing::info!(transfer = %tid, bytes, "打包下载完成"),
            Err(err) => tracing::warn!(
                transfer = %tid,
                code = err.code.as_str(),
                "打包下载失败: {}",
                err.message
            ),
        }
    });

    Ok(transfer_id)
}
