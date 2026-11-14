//! SFTP 相关 command。
//!
//! 分工：这里只做参数整理与转发，浏览/操作/传输逻辑都在 `sftp/` 下。
//! 传输是后台任务，command 立刻返回 `transferId`，进度通过 `sftp:progress` 事件推送。

use std::path::PathBuf;

use serde::Serialize;
use tauri::{Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::commands::connections::prepare_probe;
use crate::error::{AppError, AppResult};
use crate::events::EVT_SFTP_PROGRESS;
use crate::sftp::{list, ops, session, transfer, RemoteEntry};
use crate::state::AppState;
use crate::storage::known_hosts::{self, KnownHostRecord};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpOpened {
    pub session_id: String,
    /// 登录后的家目录，前端打开面板时直接进这里
    pub home: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStat {
    pub is_dir: bool,
    pub size: u64,
}

/// 打开 SFTP 面板。**独立建连**，不复用终端那条连接。
#[tauri::command]
pub async fn sftp_open(
    state: State<'_, AppState>,
    connection_id: String,
    trust_fingerprint: Option<String>,
) -> AppResult<SftpOpened> {
    let prepared = prepare_probe(
        state.inner(),
        Some(connection_id.clone()),
        None,
        trust_fingerprint,
    )
    .await?;
    let host_label = prepared.host_label.clone();
    let session_id = format!("f{}", state.next_session_seq());

    let (opened, conn) = session::open(session_id.clone(), connection_id, prepared.request).await?;
    state.sftp.insert(session_id.clone(), conn);
    // 立刻续一次租约：用户刚点开文件面板，智能体应当马上能用，
    // 不能等下一次心跳（最多 5 秒的空窗会被当成「没连上」）。
    crate::lease::publish(state.inner());

    if let Some((algo, fingerprint)) = opened.newly_trusted {
        let record = KnownHostRecord {
            host_key: host_label.clone(),
            algo,
            fingerprint: fingerprint.clone(),
        };
        state
            .db
            .call(move |conn| known_hosts::upsert(conn, &record))
            .await?;
        tracing::info!(host = %host_label, fp = %fingerprint, "已记录新主机指纹");
    }

    tracing::info!(session = %session_id, home = %opened.home, "SFTP 会话已建立");

    Ok(SftpOpened {
        session_id,
        home: opened.home,
        elapsed_ms: opened.elapsed_ms,
    })
}

/// 关闭 SFTP 面板。会先取消它名下所有在途传输，再断开连接。
#[tauri::command]
pub async fn sftp_close(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.transfers.cancel_session(&session_id);

    if let Some(conn) = state.sftp.remove(&session_id) {
        conn.close().await;
        tracing::info!(session = %session_id, "SFTP 会话已关闭");
    }
    // 断开即失效：智能体不该在用户关掉面板之后还能用这台机器。
    crate::lease::publish(state.inner());

    Ok(())
}

/// 关掉所有 SFTP 连接，返回关闭数量。
///
/// 用途：前端整页重载（WebView 重载、dev 下的 HMR 全量刷新）后，
/// 前端手里的 `sessionId` 全丢了，但后端连接还活着 —— 不清就是连接泄漏，
/// 反复重载会在服务器上堆一串没人用的 sshd 会话。
/// 只在单窗口前提下成立；将来做多窗口时要改成按窗口归属清理。
#[tauri::command]
pub async fn sftp_close_all(state: State<'_, AppState>) -> AppResult<usize> {
    // 先取消这些会话名下的在途传输，再关连接
    for id in state.sftp.ids() {
        state.transfers.cancel_session(&id);
    }

    let closed = state.sftp.close_all().await;
    crate::lease::publish(state.inner());

    if closed > 0 {
        tracing::info!(count = closed, "清理残留 SFTP 会话");
    }

    Ok(closed)
}

#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> AppResult<Vec<RemoteEntry>> {
    let conn = state.sftp.get(&session_id)?;
    list::read_dir(&conn.session, &path).await
}

/// 给「上一级」用：前端需要知道某个路径的父目录。
#[tauri::command]
pub async fn sftp_parent_of(path: String) -> AppResult<Option<String>> {
    Ok(list::parent_path(&path))
}

#[tauri::command]
pub async fn sftp_stat(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> AppResult<Option<RemoteStat>> {
    let conn = state.sftp.get(&session_id)?;
    Ok(ops::stat(&conn.session, &path)
        .await?
        .map(|(is_dir, size)| RemoteStat { is_dir, size }))
}

#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> AppResult<()> {
    let conn = state.sftp.get(&session_id)?;
    ops::create_dir(&conn.session, &path).await
}

#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    session_id: String,
    from: String,
    to: String,
) -> AppResult<()> {
    let conn = state.sftp.get(&session_id)?;
    ops::rename(&conn.session, &from, &to).await
}

/// 删除。`recursive` 必须由前端在二次确认后显式传 true。
#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    recursive: bool,
) -> AppResult<()> {
    let conn = state.sftp.get(&session_id)?;
    ops::remove(&conn.session, &path, recursive).await
}

/// 上传本地文件到远端目录。立即返回 `transferId`。
#[tauri::command]
pub async fn sftp_upload(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_dir: String,
    name: Option<String>,
) -> AppResult<String> {
    let conn = state.sftp.get(&session_id)?;
    let local = PathBuf::from(&local_path);

    // 先做一次同步校验。目录、应用数据目录这类必然失败的源，应该在这里就返回错误，
    // 而不是先给个 transferId、再让进度条上出现一条失败 —— 拖拽上传一次丢进来好几个
    // 文件时，前者是一句明确的提示，后者是一串转瞬即逝的红条。
    let protected: [&std::path::Path; 2] = [&state.paths.data_dir, &state.paths.backup_dir];
    transfer::ensure_upload_source(&local, &protected)?;

    let file_name = match name {
        Some(given) if !given.trim().is_empty() => given,
        _ => local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| AppError::validation("无法从本地路径推断文件名"))?
            .to_string(),
    };

    let remote_path = list::join_path(&remote_dir, &file_name);
    let transfer_id = new_transfer_id(state.inner(), &session_id);
    let token = CancellationToken::new();
    state.transfers.register(transfer_id.clone(), token.clone());

    let transfers = state.transfers.clone();
    let emit = progress_emitter(app, session_id.clone());
    let tid = transfer_id.clone();

    // 应用自己的数据目录：库里是加密凭据、指纹库里是主机指纹，
    // 一律不许当上传源 —— local_path 是 IPC 参数，不能假设来自文件对话框。
    let data_dir = state.paths.data_dir.clone();
    let backup_dir = state.paths.backup_dir.clone();

    tauri::async_runtime::spawn(async move {
        let protected: [&std::path::Path; 2] = [&data_dir, &backup_dir];
        let result = transfer::upload(
            &conn.session,
            &local,
            &remote_path,
            tid.clone(),
            token,
            emit,
            &protected,
        )
        .await;
        transfers.finish(&tid);

        match result {
            Ok(bytes) => tracing::info!(transfer = %tid, bytes, "上传完成"),
            Err(err) => tracing::warn!(
                transfer = %tid,
                code = err.code.as_str(),
                "上传失败: {}",
                err.message
            ),
        }
    });

    Ok(transfer_id)
}

/// 下载远端文件到本地路径。立即返回 `transferId`。
#[tauri::command]
pub async fn sftp_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> AppResult<String> {
    let conn = state.sftp.get(&session_id)?;
    let local = PathBuf::from(&local_path);

    let transfer_id = new_transfer_id(state.inner(), &session_id);
    let token = CancellationToken::new();
    state.transfers.register(transfer_id.clone(), token.clone());

    let transfers = state.transfers.clone();
    let emit = progress_emitter(app, session_id.clone());
    let tid = transfer_id.clone();

    tauri::async_runtime::spawn(async move {
        let result = transfer::download(
            &conn.session,
            &remote_path,
            &local,
            tid.clone(),
            token,
            emit,
        )
        .await;
        transfers.finish(&tid);

        match result {
            Ok(bytes) => tracing::info!(transfer = %tid, bytes, "下载完成"),
            Err(err) => tracing::warn!(
                transfer = %tid,
                code = err.code.as_str(),
                "下载失败: {}",
                err.message
            ),
        }
    });

    Ok(transfer_id)
}

/// 取消在途传输。返回 false 表示它已经结束了（按钮点晚了）。
#[tauri::command]
pub async fn sftp_cancel(state: State<'_, AppState>, transfer_id: String) -> AppResult<bool> {
    Ok(state.transfers.cancel(&transfer_id))
}

/// 传输 id 带 session 前缀，`Transfers::cancel_session` 靠它做批量取消。
fn new_transfer_id(state: &AppState, session_id: &str) -> String {
    format!("{session_id}::{}", state.next_session_seq())
}

/// 推给前端的事件载荷：进度字段 + 所属会话。
///
/// 会话页现在一个标签一个文件面板，多个面板同时监听同一个事件；
/// 没有 `sessionId` 它们分不清「这条进度是不是我的」，会互相显示对方的传输。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProgressEvent {
    session_id: String,
    #[serde(flatten)]
    progress: transfer::TransferProgress,
}

pub(crate) fn progress_emitter(app: tauri::AppHandle, session_id: String) -> transfer::ProgressFn {
    Box::new(move |payload| {
        let event = ProgressEvent {
            session_id: session_id.clone(),
            progress: payload,
        };
        let _ = app.emit(EVT_SFTP_PROGRESS, event);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 事件载荷的形状是前后端契约：前端靠 `sessionId` 区分是哪个面板的传输。
    /// 这个测试盯住字段名，改名会让前端静默收不到自己的进度。
    #[test]
    fn progress_event_carries_session_id() {
        let event = ProgressEvent {
            session_id: "f1".to_string(),
            progress: crate::sftp::transfer::TransferProgress {
                transfer_id: "f1::7".to_string(),
                direction: "upload",
                name: "a.txt".to_string(),
                transferred: 10,
                total: 100,
                bps: 5,
                state: "running",
                error: None,
            },
        };

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["sessionId"], "f1");
        // progress 被 flatten 进来，字段是同级的
        assert_eq!(value["transferId"], "f1::7");
        assert_eq!(value["state"], "running");
        assert!(
            value.get("progress").is_none(),
            "flatten 后不该再有嵌套对象"
        );
    }

    #[test]
    fn transfer_id_is_namespaced_by_session() {
        let transfers = crate::sftp::Transfers::new();
        let token = CancellationToken::new();
        transfers.register("f1::7".to_string(), token);

        // 关掉 f1 不该影响 f2 名下的传输
        let other = CancellationToken::new();
        transfers.register("f2::8".to_string(), other.clone());

        transfers.cancel_session("f1");
        assert_eq!(
            transfers.active_count(),
            2,
            "取消不是删除，注册表条目由任务自己摘"
        );
    }
}
