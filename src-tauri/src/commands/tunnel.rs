//! 本地端口转发（`-L`）的 command。
//!
//! 只做校验、拼装探测请求、调运行时与写状态；网络细节在 `ssh::tunnel`。
//! 状态变化统一走 `TunnelManager` 注入的通知口（`EVT_TUNNEL_STATE`），
//! 重连这类发生在后台任务里的事件也能发出去。

use serde::Serialize;
use tauri::State;

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
    /// 应用启动时是否自动拉起
    pub auto_start: bool,
    /// `stopped` | `listening` | `reconnecting` | `failed`
    pub state: String,
    /// 失败/重连原因（`state` 是 `failed` 或 `reconnecting` 时有值）
    pub message: Option<String>,
    pub created_at: i64,
}

impl TunnelDto {
    fn from_record(rec: &TunnelRecord, state: TunnelState) -> Self {
        let (kind, message) = match state {
            TunnelState::Stopped => ("stopped", None),
            TunnelState::Listening { .. } => ("listening", None),
            TunnelState::Reconnecting { message } => ("reconnecting", Some(message)),
            TunnelState::Failed { message } => ("failed", Some(message)),
        };
        Self {
            id: rec.id.clone(),
            connection_id: rec.connection_id.clone(),
            local_port: rec.local_port,
            target_host: rec.target_host.clone(),
            target_port: rec.target_port,
            auto_start: rec.auto_start,
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
    auto_start: Option<bool>,
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

    let rec = TunnelRecord::new(connection_id, local_port, target_host, target_port)
        .with_auto_start(auto_start.unwrap_or(false));
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
        auto_start = rec.auto_start,
        "已保存转发规则"
    );
    Ok(TunnelDto::from_record(&rec, TunnelState::Stopped))
}

/// 改一条规则的自动启动开关。开关只影响下次启动，不会立刻拉起或停掉。
#[tauri::command]
pub async fn tunnel_set_auto_start(
    state: State<'_, AppState>,
    id: String,
    auto_start: bool,
) -> AppResult<TunnelDto> {
    state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::set_auto_start(conn, &id, auto_start)
        })
        .await?;

    let rec = state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::get(conn, &id)
        })
        .await?;

    tracing::info!(tunnel = %rec.id, auto_start, "已更新自动启动开关");
    Ok(TunnelDto::from_record(&rec, state.tunnels.state(&rec.id)))
}

/// 删除规则；正在跑的先停掉。
#[tauri::command]
pub async fn tunnel_delete(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.tunnels.stop(&id);
    state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::delete(conn, &id)
        })
        .await?;
    Ok(())
}

/// 启动转发。
#[tauri::command]
pub async fn tunnel_start(state: State<'_, AppState>, id: String) -> AppResult<TunnelDto> {
    let rec = state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::get(conn, &id)
        })
        .await?;

    match start_record(state.inner(), &rec).await {
        Ok(port) => {
            tracing::info!(tunnel = %rec.id, port, "转发已就绪");
            Ok(TunnelDto::from_record(
                &rec,
                TunnelState::Listening { port },
            ))
        }
        Err(err) => {
            // 失败也要留下痕迹：前端刷新列表时要能看到"为什么没起来"。
            // 只存人话 —— 行内那行字给用户看，`[TUNNEL_FAILED]` 这种前缀是给日志的。
            let note = err
                .detail
                .clone()
                .unwrap_or_else(|| err.message.to_string());
            state.tunnels.note_failure(&rec.id, note.clone());
            state.tunnels.announce(&rec.id, "failed", Some(&note));
            Err(err)
        }
    }
}

/// 停止转发（规则保留，下次还能点启动）。
#[tauri::command]
pub async fn tunnel_stop(state: State<'_, AppState>, id: String) -> AppResult<TunnelDto> {
    state.tunnels.stop(&id);
    let rec = state
        .db
        .call({
            let id = id.clone();
            move |conn| tunnels::get(conn, &id)
        })
        .await?;
    Ok(TunnelDto::from_record(&rec, TunnelState::Stopped))
}

/// 拉起所有打开了自动启动的规则。
///
/// 启动时机由 `lib.rs` 在窗口起来后延迟触发 —— 这时网络栈才就绪，
/// 开机自启的场景不至于每一条都失败。失败只记状态，不阻断其它规则。
pub async fn apply_auto_start(state: &AppState) -> usize {
    let rows = match state.db.call(tunnels::list_auto_start).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err.message, "读取自动启动规则失败");
            return 0;
        }
    };

    let mut started = 0usize;
    for rec in rows {
        // 手动点过、已经在跑的别重复建连
        if matches!(state.tunnels.state(&rec.id), TunnelState::Listening { .. }) {
            started += 1;
            continue;
        }
        match start_record(state, &rec).await {
            Ok(port) => {
                started += 1;
                tracing::info!(tunnel = %rec.id, port, "自动启动转发成功");
            }
            Err(err) => {
                let note = err
                    .detail
                    .clone()
                    .unwrap_or_else(|| err.message.to_string());
                state.tunnels.note_failure(&rec.id, note.clone());
                tracing::warn!(
                    tunnel = %rec.id,
                    target = %format!("{}:{}", rec.target_host, rec.target_port),
                    error = %note,
                    "自动启动转发失败"
                );
            }
        }
    }
    started
}

/// 建连 + 开监听。状态通知由 `TunnelManager` 内部发出，这里不碰事件。
async fn start_record(state: &AppState, rec: &TunnelRecord) -> AppResult<u16> {
    // 用这条连接（含它的跳板链）建隧道：指纹、凭据、兼容算法全都沿用原来那套
    let prepared = prepare_probe(state, Some(rec.connection_id.clone()), None, None).await?;

    let request = StartRequest {
        tunnel_id: rec.id.clone(),
        connection_id: rec.connection_id.clone(),
        local_port: rec.local_port,
        target_host: rec.target_host.clone(),
        target_port: rec.target_port,
        probe: prepared.request,
    };

    state.tunnels.start(request).await
}
