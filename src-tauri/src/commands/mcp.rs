//! MCP 白名单的 GUI 接口。
//!
//! `mcp.json` 是唯一的事实来源：GUI 只是读写它，MCP 进程自己按 mtime 热加载，
//! 两边不需要 IPC、不需要 GUI 常驻。
//!
//! 前端不直接编辑 JSON，但仍然要在**服务端**再校验一遍 —— 配置决定安全边界，
//! 不能假设调用方是善意的（或者没被改坏）。

use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::mcp::audit::{AuditEntry, AuditLog};
#[cfg(test)]
use crate::mcp::config::ConnectionPolicy;
use crate::mcp::config::{ConfigStore, McpConfig, ALL_TOOLS, TOOL_EXEC};
use crate::mcp::exec::validate_command;
use crate::state::AppState;

/// 常用只读命令模板。给设置页做成可勾选的按钮，人不用手敲命令。
pub const COMMAND_TEMPLATES: &[&str] = &[
    "df -h",
    "free -m",
    "uptime",
    "systemctl status",
    "journalctl -n 50",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionBrief {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigView {
    pub config: McpConfig,
    /// 库里的全部连接（含没启用的），供前端列出来勾
    pub connections: Vec<ConnectionBrief>,
    pub command_templates: Vec<String>,
    pub config_path: String,
    pub audit_path: String,
}

/// 读取当前白名单 + 连接清单。
#[tauri::command]
pub async fn mcp_config_get(state: State<'_, AppState>) -> AppResult<McpConfigView> {
    let rows = state
        .db
        .call(|conn| crate::storage::connections::list(conn, None))
        .await?;

    let connections = rows
        .iter()
        .map(|record| ConnectionBrief {
            id: record.id.clone(),
            name: record.name.clone(),
            host: record.host.clone(),
            port: record.port,
        })
        .collect();

    let store = ConfigStore::new(&state.paths.product_dir);
    let audit = AuditLog::new(&state.paths.product_dir);

    Ok(McpConfigView {
        config: store.load(),
        connections,
        command_templates: COMMAND_TEMPLATES.iter().map(|s| s.to_string()).collect(),
        config_path: store.path().display().to_string(),
        audit_path: audit.path().display().to_string(),
    })
}

/// 全量写回白名单。写之前在服务端再校验一遍。
#[tauri::command]
pub fn mcp_config_set(state: State<'_, AppState>, config: McpConfig) -> AppResult<()> {
    validate_config(&config)?;
    ConfigStore::new(&state.paths.product_dir).save(&config)
}

/// 最近被拒绝的调用，给「一键允许」用。
#[tauri::command]
pub fn mcp_audit_recent(state: State<'_, AppState>) -> Vec<AuditEntry> {
    AuditLog::new(&state.paths.product_dir).recent_denied(20)
}

/// 把一条被拒的命令加进白名单。
///
/// 只做这一件事：加命令 + 确保 `exec` 工具开着。不会顺手启用连接、不会放开路径，
/// 免得点一下把别的口子也打开了。
#[tauri::command]
pub fn mcp_allow_command(
    state: State<'_, AppState>,
    connection: String,
    command: String,
) -> AppResult<()> {
    if validate_command(&command, std::slice::from_ref(&command)).is_err() {
        return Err(AppError::validation(
            "这条命令含复合字符或格式不合法，不能加入白名单",
        ));
    }

    let store = ConfigStore::new(&state.paths.product_dir);
    let mut config = store.load();
    let policy = config.connections.entry(connection.clone()).or_default();

    if !policy.tools.iter().any(|tool| tool == TOOL_EXEC) {
        policy.tools.push(TOOL_EXEC.into());
    }
    if !policy.exec.allow.iter().any(|item| item == &command) {
        policy.exec.allow.push(command);
    }
    store.save(&config)
}

/// 配置决定安全边界，服务端必须自己再验一次。
fn validate_config(config: &McpConfig) -> AppResult<()> {
    if config.version != 1 {
        return Err(AppError::validation("不支持的 MCP 配置版本"));
    }

    for (key, policy) in &config.connections {
        for tool in &policy.tools {
            if !ALL_TOOLS.contains(&tool.as_str()) {
                return Err(AppError::validation(format!("{key} 配了未知工具 {tool}")));
            }
        }
        for path in &policy.paths {
            if !path.starts_with('/') {
                return Err(AppError::validation(format!(
                    "{key} 的可见路径必须是绝对路径：{path}"
                )));
            }
        }
        for command in &policy.exec.allow {
            // 拿命令自己当白名单去验证，正好复用复合字符检查
            if let Err(err) = validate_command(command, std::slice::from_ref(command)) {
                return Err(AppError::validation(format!(
                    "{key} 的命令 {command} 不合法（{}）",
                    err.as_str()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tool_is_rejected() {
        let mut cfg = McpConfig::default();
        cfg.connections.insert(
            "a".into(),
            ConnectionPolicy {
                tools: vec!["rm_everything".into()],
                ..Default::default()
            },
        );
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn relative_path_is_rejected() {
        let mut cfg = McpConfig::default();
        cfg.connections.insert(
            "a".into(),
            ConnectionPolicy {
                paths: vec!["etc/passwd".into()],
                ..Default::default()
            },
        );
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn compound_command_in_allowlist_is_rejected() {
        let mut cfg = McpConfig::default();
        cfg.connections.insert(
            "a".into(),
            ConnectionPolicy {
                exec: crate::mcp::config::ExecPolicy {
                    allow: vec!["df -h; rm -rf /".into()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn valid_config_passes() {
        let mut cfg = McpConfig::default();
        cfg.connections.insert(
            "a".into(),
            ConnectionPolicy {
                enabled: true,
                tools: vec![TOOL_EXEC.into()],
                paths: vec!["/var/log".into()],
                exec: crate::mcp::config::ExecPolicy {
                    allow: vec!["df -h".into()],
                    ..Default::default()
                },
            },
        );
        assert!(validate_config(&cfg).is_ok());
    }
}
