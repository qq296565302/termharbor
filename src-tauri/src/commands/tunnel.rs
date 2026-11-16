//! 本地端口转发（`-L`）的 command。
//!
//! 只做校验、拼装探测请求、调运行时与写状态；网络细节在 `ssh::tunnel`。

use serde::Serialize;
use tauri::{Emitter, State};

use crate::commands::connections::prepare_probe;
use crate::error::{AppError, AppResult};
use crate::ssh::tunnel::StartRequest;
use crate::ssh::TunnelState;
use crate::state::AppState;
use crate::storage::tunnels::{self, TunnelRecord};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelDto {
    pub id: String,
    pub connection_id: String,
    /// 本机监听端口（地址固定 127.0.0.1）
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// `stopped` | `listening` | `failed`
    pub state: String,
    /// 失败原因（`state == failed` 时有值）
    pub message: Option<String>,
    pub created_at: i64,
}

impl TunnelDto {
    fn from_record(rec: &TunnelRecord, state: TunnelState) -> Self {
        let (kind, message) = match state {
            TunnelState::Stopped => ("stopped", None),
            TunnelState::Listening { .. } => ("listening", None),
            TunnelState::Failed { message } => ("failed", Some(message)),
        };
        Self {
            id: rec.id.clone(),
            connection_id: rec.connection_id.clone(),
            local_port: rec.local_port,
            target_host: rec.target_host.clone(),
            target_port: rec.target_port,
            state: kind.to_string(),
            message,
            created_at: rec.created_at,
        }
    }
}

/// 列出转发规则（带运行状态）。`connection_id` 为空表示全部。
#[tauri::command]
pub async fn tunnel_list(
    state: State<'_, AppState>,
    connection_id: Option<String>,
) -> AppResult<Vec<TunnelDto>> {
    let rows = state
        .db
        .call(move |conn| tunnels::list(conn, connection_id.as_deref()))
        .await?;

    Ok(rows
        .iter()
        .map(|rec| TunnelDto::from_record(rec, state.tunnels.state(&rec.id)))
        .collect())
}

/// 新增一条转发规则（只落库，不启动）。
#[tauri::command]
pub async fn tunnel_create(
    state: State<'_, AppState>,
    connection_id: String,
    local_port: u16,
    target_host: String,
    target_port: u16,
) -> AppResult<TunnelDto> {
    let target_host = target_host.trim().to_string();
    if local_port < 1 {
        return Err(AppError::validation("本机端口必须在 1–65535 之间"));
    }
    if target_port < 1 {
        return Err(AppError::validation("目标端口必须在 1–65535 之间"));
    }
    if target_host.is_empty() {
        return Err(AppError::validation("目标地址不能为空"));
    }
    if target_host.chars().any(char::is_whitespace) {
        return Err(AppError::validation("目标地址不能包含空白字符"));
    }

    // 连接得存在（不然规则挂在一个不存在的 id 上）
    state
        .db
        .call({
            let id = connection_id.clone();
            move |conn| crate::storage::connections::get(conn, &id)
        })
        .await?;

    let rec = TunnelRecord::new(connection_id, local_port, target_host, target_port);
    state
        .db
        .call({
            let rec = rec.clone();
            move |conn| tunnels::insert(conn, &rec)
        })
        .await?;

    tracing::info!(
        tunnel = %rec.id,
        local = rec.local_port,
        target = %format!("{}:{}", rec.target_host, rec.target_port),
        "已保存转发规则"
    );
    Ok(TunnelDto::from_record(&rec, TunnelState::Stopped))
}

/// 删除规则；正在跑的先停掉。
#[tauri::command]
pub async fn tunnel_delete(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<()> {
    state.tunnels.stop(&id);
    state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::delete(conn, &id)
        })
        .await?;

    let _ = app.emit(
        crate::events::EVT_TUNNEL_STATE,
        serde_json::json!({ "tunnelId": id, "state": "stopped" }),
    );
    Ok(())
}

/// 启动转发。
#[tauri::command]
pub async fn tunnel_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<TunnelDto> {
    let rec = state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::get(conn, &id)
        })
        .await?;

    // 用这条连接（含它的跳板链）建隧道：指纹、凭据、兼容算法全都沿用原来那套
    let prepared =
        prepare_probe(state.inner(), Some(rec.connection_id.clone()), None, None).await?;

    let request = StartRequest {
        tunnel_id: rec.id.clone(),
        connection_id: rec.connection_id.clone(),
        local_port: rec.local_port,
        target_host: rec.target_host.clone(),
        target_port: rec.target_port,
        probe: prepared.request,
    };

    match state.tunnels.start(request).await {
        Ok(port) => {
            tracing::info!(tunnel = %rec.id, port, "转发已就绪");
            let _ = app.emit(
                crate::events::EVT_TUNNEL_STATE,
                serde_json::json!({ "tunnelId": rec.id, "state": "listening", "port": port }),
            );
            Ok(TunnelDto::from_record(
                &rec,
                TunnelState::Listening { port },
            ))
        }
        Err(err) => {
            // 失败也要留下痕迹：前端刷新列表时要能看到"为什么没起来"
            state.tunnels.note_failure(&rec.id, err.to_string());
            let _ = app.emit(
                crate::events::EVT_TUNNEL_STATE,
                serde_json::json!({ "tunnelId": rec.id, "state": "failed", "message": err.to_string() }),
            );
            Err(err)
        }
    }
}

/// 停止转发（规则保留，下次还能点启动）。
#[tauri::command]
pub async fn tunnel_stop(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<TunnelDto> {
    state.tunnels.stop(&id);
    let rec = state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::get(conn, &id)
        })
        .await?;

    let _ = app.emit(
        crate::events::EVT_TUNNEL_STATE,
        serde_json::json!({ "tunnelId": rec.id, "state": "stopped" }),
    );
    Ok(TunnelDto::from_record(&rec, TunnelState::Stopped))
}
