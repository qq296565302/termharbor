//! 结构化诊断 command。
//!
//! 走终端会话同一条 SSH 连接的独立 exec 通道，不碰 PTY。

use tauri::State;

use crate::error::AppResult;
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
