//! 命令片段 command。
//!
//! 校验、风险判定、过滤、渲染都在 `snippets` 模块里，这边只做编排和落库。

use serde::Deserialize;
use tauri::State;

use crate::error::AppResult;
use crate::snippets::{self, CustomSnippet, EnvStatus, RiskLevel, SnippetDto};
use crate::state::AppState;
use crate::storage::snippets as store;

/// 新建或修改自定义片段。`id` 为空表示新建。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetInput {
    pub id: Option<String>,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    /// 分组名。用户拿它放项目名（「web 项目」「api 服务」），列表就按项目折叠
    pub group: Option<String>,
}

/// 列出片段：自定义在前，内置在后。
#[tauri::command]
pub async fn snippet_list(
    state: State<'_, AppState>,
    query: Option<String>,
) -> AppResult<Vec<SnippetDto>> {
    let custom = state.db.call(store::load_all).await?;
    Ok(snippets::list(query.as_deref(), &custom))
}

/// 保存自定义片段，返回落库后的完整条目（风险已判定）。
#[tauri::command]
pub async fn snippet_save(
    state: State<'_, AppState>,
    input: SnippetInput,
) -> AppResult<SnippetDto> {
    let (name, command, description) = snippets::validate_custom(
        &input.name,
        &input.command,
        input.description.as_deref().unwrap_or(""),
    )?;

    let group = input.group.unwrap_or_default().trim().to_string();
    let id = input
        .id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("c-{}", uuid::Uuid::now_v7()));

    let risk = snippets::classify_risk(&command);
    let item = CustomSnippet {
        id: id.clone(),
        name: name.clone(),
        command: command.clone(),
        description: description.clone(),
        group: group.clone(),
    };

    let saved = item.clone();
    let risk_level = risk.as_str().to_string();
    state
        .db
        .call(move |conn| {
            store::upsert(conn, &saved, &risk_level, crate::state::now_millis() as i64)
        })
        .await?;

    tracing::info!(snippet = %id, risk = risk.as_str(), "片段已保存");

    Ok(SnippetDto {
        id,
        name,
        command,
        description,
        group: if group.is_empty() {
            "我的片段".to_string()
        } else {
            group
        },
        risk,
        diag: None,
        builtin: false,
    })
}

/// 删除自定义片段。内置片段删不掉（SQL 里有 `is_builtin = 0` 兜着）。
#[tauri::command]
pub async fn snippet_delete(state: State<'_, AppState>, id: String) -> AppResult<bool> {
    state.db.call(move |conn| store::delete(conn, &id)).await
}

/// 只判风险，用于输入时实时提示等级（前端不得自己判，SEC-SN-10）。
#[tauri::command]
pub async fn snippet_classify(command: String) -> AppResult<RiskLevel> {
    Ok(snippets::classify_risk(&command))
}

/// 取片段最终要写进终端的文本。
///
/// 前端拿到后只写入终端、**不自动回车**（A-12）。
/// 走这一层而不是直接用 list 里的 command，是为了让「渲染 + 校验」留在 Rust 侧（A-11），
/// M7 加变量替换时前端不用改。
#[tauri::command]
pub async fn snippet_render(state: State<'_, AppState>, id: String) -> AppResult<String> {
    // 自定义片段的 id 不在内置表里，先去库里找
    if let Ok(text) = snippets::render(&id) {
        return Ok(text);
    }

    let custom = state.db.call(store::load_all).await?;
    let found = custom
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| crate::error::AppError::validation(format!("片段不存在: {id}")))?;

    snippets::validate_custom(&found.name, &found.command, &found.description)?;
    Ok(found.command)
}

/// 探测服务器上已经装了哪些运行时。
///
/// 走终端会话的连接开一个临时 exec 通道，不占用 PTY。
/// 前端拿结果在侧栏标出「已装 / 未装」，用户才知道该点哪条安装命令。
#[tauri::command]
pub async fn env_probe(
    state: State<'_, AppState>,
    session_id: String,
) -> AppResult<Vec<EnvStatus>> {
    let conn = state.sessions.connection(&session_id)?;
    let raw = crate::ssh::exec::exec_once(&conn, snippets::PROBE_COMMAND).await?;
    Ok(snippets::parse_probe(&raw))
}
