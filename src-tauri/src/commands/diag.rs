//! 结构化诊断 command。
//!
//! 走终端会话同一条 SSH 连接的独立 exec 通道，不碰 PTY。

use tauri::State;

use crate::error::AppResult;
use crate::monitor::load::{self, LoadSnapshot};
use crate::monitor::{self, DiagKind, DiagResult};
use crate::state::AppState;

/// 跑一条诊断，返回结构化结果。
#[tauri::command]
pub async fn diag_run(
    state: State<'_, AppState>,
    session_id: String,
    kind: DiagKind,
) -> AppResult<DiagResult> {
    let conn = state.sessions.connection(&session_id)?;
    monitor::run(&conn, kind).await
}

/// 采一次服务器负载快照（看板用）。
///
/// 一次 exec 通道往返采全部指标；命令里 sleep 0.5 秒采两帧，
/// 用来算 CPU 使用率和网络速率 —— 这些都是累计计数器，差值才有意义。
#[tauri::command]
pub async fn load_snapshot(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<LoadSnapshot> {
    let conn = state.sessions.connection(&session_id)?;
    let raw = crate::ssh::exec::exec_once(&conn, load::SAMPLE_COMMAND).await?;
    Ok(load::parse(&raw))
}
