//! 终端会话 command。
//!
//! 与连接测试共用 `prepare_probe`，所以"能测通"和"能开终端"走的是同一套凭据与指纹策略。

use tauri::ipc::Channel;
use tauri::{Emitter, State};

use crate::commands::connections::prepare_probe;
use crate::error::AppResult;
use crate::events::OutputBatch;
use crate::ssh::session::{self, OpenRequest, SessionInfo};
use crate::state::AppState;
use crate::storage::known_hosts::{self, KnownHostRecord};

/// 打开一个交互式终端会话。
///
/// `channel` 是输出流；调用方必须保证终端已经完成 fit，否则 PTY 尺寸会不对。
#[tauri::command]
pub async fn session_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    cols: u32,
    rows: u32,
    trust_fingerprint: Option<String>,
    channel: Channel<OutputBatch>,
) -> AppResult<SessionInfo> {
    let prepared = prepare_probe(
        state.inner(),
        Some(connection_id.clone()),
        None,
        trust_fingerprint,
    )
    .await?;

    let request = OpenRequest {
        session_id: format!("s{}", state.next_session_seq()),
        connection_id,
        title: prepared.display_name,
        host_label: prepared.host_label.clone(),
        cols,
        rows,
        probe: prepared.request,
    };

    // 把 Tauri 的通道与事件都包成回调，ssh 层不需要知道 Tauri 的存在
    let emit: session::EmitFn = Box::new(move |batch| channel.send(batch).is_ok());
    let notify: session::EventFn = Box::new(move |event| {
        let session::SessionEvent::Disconnected { session_id, reason } = event;
        let _ = app.emit(
            crate::events::EVT_SSH_STATE,
            serde_json::json!({
                "sessionId": session_id,
                "state": "disconnected",
                "reason": reason,
            }),
        );
    });
    let opened = session::open(state.sessions.clone(), request, emit, notify).await?;

    if let Some((algo, fingerprint)) = opened.newly_trusted {
        let record = KnownHostRecord {
            host_key: prepared.host_label.clone(),
            algo,
            fingerprint: fingerprint.clone(),
        };
        state
            .db
            .call(move |conn| known_hosts::upsert(conn, &record))
            .await?;
        tracing::info!(host = %prepared.host_label, fp = %fingerprint, "已记录新主机指纹");
    }

    // 会话建成后立刻续租约：用户刚连上，智能体就该马上能用，
    // 不用等下一次心跳（那会有最多 5 秒的空窗）。
    crate::lease::publish(state.inner());

    Ok(opened.info)
}

/// 把键盘输入写进会话。
///
/// 前端传的是 `onData` 拿到的字符串；控制字符（`\r`、`\x03`）都是 ASCII，
/// UTF-8 编码后与原始字节一致，中文则是正确的多字节序列。
#[tauri::command]
pub async fn session_write(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> AppResult<()> {
    state.sessions.send_input(&session_id, data.into_bytes())
}

#[tauri::command]
pub async fn session_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> AppResult<()> {
    state.sessions.resize(&session_id, cols, rows)
}

/// 关闭会话。幂等：重复调用不报错。
#[tauri::command]
pub async fn session_close(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.sessions.close(&session_id);
    // 断开即失效：用户关掉终端标签之后，这台机器就不该再让智能体用。
    crate::lease::publish(state.inner());
    Ok(())
}

/// 关掉所有终端会话，返回关闭数量。
///
/// 前端整页重载后它手里的 sessionId 全丢了，但 PTY 还开着。
/// 不清理的话，每次重载都在服务器上留一个没人用的 shell。
#[tauri::command]
pub async fn session_close_all(state: State<'_, AppState>) -> AppResult<usize> {
    let closed = state.sessions.close_all();
    crate::lease::publish(state.inner());
    Ok(closed)
}

/// 前端消费完一段输出后回调，用于解除背压。
#[tauri::command]
pub async fn session_ack(
    state: State<'_, AppState>,
    session_id: String,
    bytes: u64,
) -> AppResult<()> {
    state.sessions.ack(&session_id, bytes);
    Ok(())
}

#[tauri::command]
pub async fn session_list(state: State<'_, AppState>) -> AppResult<Vec<SessionInfo>> {
    Ok(state.sessions.list())
}
