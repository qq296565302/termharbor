//! 系统级 command：连通性自检。
//!
//! M0 的验收就靠这几个：前端能拿到版本、schema 版本、密钥来源，
//! 并且 Channel 能持续把心跳推到渲染层 —— 证明 Command + Channel 两条链路都是通的。

use std::time::Duration;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use crate::error::AppResult;
use crate::state::{now_millis, AppState};

/// 自检结果。前端「自检」页直接渲染这个结构。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pong {
    pub ok: bool,
    pub product: &'static str,
    pub version: &'static str,
    pub schema_version: i32,
    pub latest_schema_version: i32,
    pub key_source: &'static str,
    pub locked: bool,
    pub data_dir: String,
    pub log_dir: String,
    pub server_time: u64,
}

#[tauri::command]
pub async fn ping(state: State<'_, AppState>) -> AppResult<Pong> {
    let schema_version = state
        .db
        .call(|conn| Ok(conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))?))
        .await?;

    tracing::debug!(schema_version, locked = state.is_locked(), "自检请求");

    Ok(Pong {
        ok: !state.is_locked(),
        product: "TermHarbor 终端港",
        version: env!("CARGO_PKG_VERSION"),
        schema_version,
        latest_schema_version: crate::storage::migrations::latest_version(),
        key_source: state.key_source.as_str(),
        locked: state.is_locked(),
        data_dir: state.paths.data_dir.display().to_string(),
        log_dir: state.paths.log_dir.display().to_string(),
        server_time: now_millis(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Heartbeat {
    pub tick: u64,
    pub ts: u64,
}

/// 启动心跳流。仅用于 M0 验证 Channel 通道可用，M2 会被终端输出流取代。
#[tauri::command]
pub async fn start_heartbeat(
    channel: Channel<Heartbeat>,
    state: State<'_, AppState>,
) -> AppResult<()> {
    stop_heartbeat_task(state.inner());

    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(1000));
        let mut tick: u64 = 0;
        loop {
            ticker.tick().await;
            tick += 1;
            // send 失败通常意味着窗口已关闭，直接退出，避免任务泄漏
            if channel
                .send(Heartbeat {
                    tick,
                    ts: now_millis(),
                })
                .is_err()
            {
                tracing::debug!(tick, "心跳 Channel 已关闭，任务退出");
                break;
            }
        }
    });

    *state.heartbeat.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    tracing::debug!("心跳已启动");
    Ok(())
}

#[tauri::command]
pub async fn stop_heartbeat(state: State<'_, AppState>) -> AppResult<()> {
    stop_heartbeat_task(state.inner());
    Ok(())
}

fn stop_heartbeat_task(state: &AppState) {
    let existing = state
        .heartbeat
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(handle) = existing {
        handle.abort();
        tracing::debug!("心跳已停止");
    }
}
