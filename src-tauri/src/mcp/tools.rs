//! 工具清单与实现。
//!
//! 四道闸门全部在**服务端**：连接是否启用 → 工具是否允许 → 参数（路径/命令）→ 执行限额。
//! agent 传什么参数都绕不过去，因为它不参与任何放行判断。
//!
//! 错误分三层，别混：
//! - **协议错误**（`RpcError`）：参数不对、工具名不认识 —— 调用方写错了。
//! - **白名单拒绝**（`Outcome::Denied`）：配置没开 —— 明确告诉模型"找管理员开"。
//! - **执行失败**（`isError` 的 content）：连不上、没权限、目录不存在 —— 让模型据此改口。

use serde::Serialize;
use serde_json::{json, Value};

use super::config::{
    ConnectionPolicy, McpConfig, ALL_TOOLS, TOOL_EXEC, TOOL_LIST_CONNECTIONS, TOOL_LOAD_SNAPSHOT,
    TOOL_SFTP_LIST,
};
use super::context::Context;
use super::exec::{validate_command, DenyReason};
use super::jsonrpc::{RpcError, INTERNAL_ERROR, INVALID_PARAMS};
use crate::error::{AppError, ErrCode};
use crate::monitor::load::LoadSnapshot;
use crate::redact::redact;
use crate::state::now_millis;
use crate::storage::connections::ConnectionRecord;

/// 一次最多返回多少条目录项。
///
/// 远端有个 5 万文件的目录时，全塞给模型既没用又贵。超出部分明确告诉它"被截断了"，
/// 比让它以为目录就这么点内容安全。
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// 工具清单。**顺序稳定**，方便宿主端做 diff 和快照测试。
pub fn list() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: TOOL_LIST_CONNECTIONS,
            description: "列出已对 MCP 启用的服务器连接。只返回名称、主机、端口、用户名、\
                          认证方式，不含任何密码、私钥或主机指纹。\
                          只是「已保存的连接」，不检测是否在线、也不代表能登录成功。",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_LIST,
            description: "列出某台服务器上某个目录的内容。只读。\
                          只能访问本地白名单里允许的目录，超出会被拒绝。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "path": { "type": "string", "description": "远端绝对路径，例如 /var/log" }
                },
                "required": ["connection", "path"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_LOAD_SNAPSHOT,
            description: "取某台服务器的负载快照（CPU、内存、磁盘、网络、进程数）。只读。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" }
                },
                "required": ["connection"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_EXEC,
            description: "在该服务器上执行一条**本地白名单里预先允许**的只读命令。\
                          复合命令（;、管道、重定向等）一律拒绝；未授权的命令会被拒绝。\
                          调用前先用 list_connections 确认连接可用。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "command": { "type": "string", "description": "单条命令，例如 df -h" }
                },
                "required": ["connection", "command"],
                "additionalProperties": false
            }),
        },
    ]
}

/// 一次调用的三种结局。审计日志据此写一条，模型据此看到不同文案。
enum Outcome {
    Ok {
        value: Value,
        connection: String,
    },
    Denied {
        connection: String,
        reason: DenyReason,
    },
    Error {
        connection: String,
        err: AppError,
    },
}

/// 分发一次工具调用，统一做审计。
pub async fn call(ctx: &Context, name: &str, arguments: Option<&Value>) -> Result<Value, RpcError> {
    let started = now_millis();
    // exec 的审计参数记**命令本身**而不是整包 JSON：设置页「允许这条」要用它，
    // 记成 JSON 就没法直接加进白名单了。
    let args_desc = if name == TOOL_EXEC {
        arguments
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        arguments.map(ToString::to_string).unwrap_or_default()
    };
    let client = ctx.client();

    if !ALL_TOOLS.contains(&name) {
        let available = ALL_TOOLS.join(", ");
        ctx.audit().record(&denied_entry(
            name,
            "-",
            &args_desc,
            &client,
            "unknown_tool",
        ));
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("未知工具 {name}；可用工具：{available}"),
        ));
    }

    let result: Result<Outcome, RpcError> = match name {
        TOOL_LIST_CONNECTIONS => list_connections(ctx).await,
        TOOL_SFTP_LIST => sftp_list(ctx, arguments).await,
        TOOL_LOAD_SNAPSHOT => load_snapshot(ctx, arguments).await,
        TOOL_EXEC => exec_command(ctx, arguments).await,
        _ => unreachable!("闸门已经过滤过工具名"),
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(err) => {
            ctx.audit().record(&denied_entry(
                name,
                "-",
                &args_desc,
                &client,
                "invalid_params",
            ));
            return Err(err);
        }
    };

    let entry = match &outcome {
        Outcome::Ok { value, connection } => {
            let bytes = value["content"][0]["text"]
                .as_str()
                .map(str::len)
                .unwrap_or(0);
            let is_err = value["isError"] == true;
            super::audit::AuditEntry::allowed(name, connection, &args_desc, &client).finish(
                started,
                bytes,
                if is_err { "error" } else { "ok" },
            )
        }
        Outcome::Denied {
            connection, reason, ..
        } => denied_entry(name, connection, &args_desc, &client, reason.as_str()),
        Outcome::Error { connection, err } => {
            let mut entry = super::audit::AuditEntry::allowed(
                name, connection, &args_desc, &client,
            )
            .finish(started, 0, redact(&describe(err)));
            entry.result = format!("error: {}", redact(&describe(err)));
            entry
        }
    };
    ctx.audit().record(&entry);

    match outcome {
        Outcome::Ok { value, .. } => Ok(value),
        // 连接没开放时**不承认它存在**：否则 agent 可以通过错误信息枚举
        // 数据库里有哪些机器。审计里仍如实记 connection_disabled。
        Outcome::Denied {
            reason: DenyReason::ConnectionDisabled,
            ..
        } => Err(RpcError::new(
            INVALID_PARAMS,
            "没有这个连接，或它没有对 MCP 开放。可用连接请用 list_connections 查看；\
             需要开放请到「设置 → MCP 接入」勾选。",
        )),
        Outcome::Denied { reason, .. } => Ok(failed(format!(
            "已被本地白名单拒绝（{}）。这个工具或路径没有对 MCP 开放。\
             请让管理员在「设置 → MCP 接入」里开启；不要尝试改用别的方式达到同样目的。",
            reason.as_str()
        ))),
        Outcome::Error { err, .. } => Ok(backend_failure(&err)),
    }
}

fn denied_entry(
    tool: &str,
    connection: &str,
    args: &str,
    client: &str,
    reason: &str,
) -> super::audit::AuditEntry {
    super::audit::AuditEntry::denied(tool, connection, args, client, reason)
}

/// 连接启用 + 工具允许，两道闸门一起过。
fn gated(
    cfg: &McpConfig,
    record: &ConnectionRecord,
    tool: &str,
) -> Result<ConnectionPolicy, DenyReason> {
    let policy = cfg
        .policy_for(&record.id, &record.name)
        .ok_or(DenyReason::ConnectionDisabled)?;
    if !policy.enabled {
        return Err(DenyReason::ConnectionDisabled);
    }
    if !cfg.allows_tool(policy, tool) {
        return Err(DenyReason::ToolNotAllowed);
    }
    Ok(policy.clone())
}

async fn list_connections(ctx: &Context) -> Result<Outcome, RpcError> {
    let cfg = ctx.config().load();
    match load_connections(ctx).await {
        Ok(rows) => {
            // 只挑能给的字段；而且只列**已启用**的连接 —— 没开的连接对 MCP 不存在。
            let items: Vec<Value> = rows
                .iter()
                .filter(|record| cfg.is_visible(&record.id, &record.name))
                .map(|record| {
                    json!({
                        "name": record.name,
                        "host": record.host,
                        "port": record.port,
                        "username": record.username,
                        "authType": record.auth_type.as_str(),
                        "lastUsedAt": record.last_used_at,
                    })
                })
                .collect();

            Ok(Outcome::Ok {
                value: text(serde_json::to_string_pretty(&items).unwrap_or_default()),
                connection: "-".into(),
            })
        }
        Err(err) => Ok(Outcome::Error {
            connection: "-".into(),
            err,
        }),
    }
}

async fn load_connections(ctx: &Context) -> crate::error::AppResult<Vec<ConnectionRecord>> {
    ctx.db()?
        .call(|conn| crate::storage::connections::list(conn, None))
        .await
}

async fn sftp_list(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let path = required_str(&args, "path")?;

    if !path.starts_with('/') {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("path 必须是远端绝对路径（以 / 开头），收到：{path}"),
        ));
    }

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_LIST) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if !McpConfig::allows_path(&policy, &path) {
        return Ok(denied(&record, DenyReason::PathNotAllowed));
    }

    match list_remote(ctx, &record.id, &path).await {
        Ok(rows) => {
            let total = rows.len();
            let truncated = total > MAX_ENTRIES;
            let items: Vec<Value> = rows
                .iter()
                .take(MAX_ENTRIES)
                .map(|entry| {
                    json!({
                        "name": entry.name,
                        "path": entry.path,
                        "isDir": entry.is_dir,
                        "isSymlink": entry.is_symlink,
                        "size": entry.size,
                        "modifiedMs": entry.modified_ms,
                    })
                })
                .collect();

            let mut header = format!("{path} 共 {total} 项");
            if truncated {
                header.push_str(&format!("，只返回前 {MAX_ENTRIES} 项"));
            }
            Ok(Outcome::Ok {
                value: text(format!(
                    "{header}\n\n{}",
                    serde_json::to_string_pretty(&items).unwrap_or_default()
                )),
                connection: record.name,
            })
        }
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

/// 真连一次服务器、列一个目录、然后断开。
async fn list_remote(
    ctx: &Context,
    id: &str,
    path: &str,
) -> crate::error::AppResult<Vec<crate::sftp::RemoteEntry>> {
    let state = ctx.app_state()?;
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;

    let (_opened, conn) =
        crate::sftp::session::open(ctx.next_session_id(), prepared.request).await?;
    let rows = crate::sftp::list::read_dir(&conn.session, path).await;
    conn.close().await;
    rows
}

/// 采一次负载快照。走 `monitor` 里同一条采样命令，所以数字和界面看板一致。
async fn load_snapshot(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    if let Err(reason) = gated(&cfg, &record, TOOL_LOAD_SNAPSHOT) {
        return Ok(denied(&record, reason));
    }

    match sample_load(ctx, &record.id).await {
        Ok(snapshot) => Ok(Outcome::Ok {
            value: text(serde_json::to_string_pretty(&snapshot).unwrap_or_default()),
            connection: record.name,
        }),
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn sample_load(ctx: &Context, id: &str) -> crate::error::AppResult<LoadSnapshot> {
    let state = ctx.app_state()?;
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;
    let raw = crate::ssh::exec::exec_on_fresh_connection(
        prepared.request,
        crate::monitor::load::SAMPLE_COMMAND,
    )
    .await?;
    Ok(crate::monitor::load::parse(&raw))
}

/// 执行档：白名单命令 + 复合字符拒 + 配置里的超时/输出上限。
async fn exec_command(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let command = required_str(&args, "command")?;
    let record = resolve_record(ctx, &spec).await?;

    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_EXEC) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };

    // 复合字符检查在白名单匹配之前，顺序不能反
    if let Err(reason) = validate_command(&command, &policy.exec.allow) {
        return Ok(denied(&record, reason));
    }

    match run_exec(ctx, &record.id, &command, &policy.exec).await {
        Ok(output) => Ok(Outcome::Ok {
            value: text(output),
            connection: record.name,
        }),
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn run_exec(
    ctx: &Context,
    id: &str,
    command: &str,
    policy: &super::config::ExecPolicy,
) -> crate::error::AppResult<String> {
    let state = ctx.app_state()?;
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;
    crate::ssh::exec::exec_on_fresh_connection_limited(
        prepared.request,
        command,
        policy.timeout(),
        policy.max_output_bytes(),
    )
    .await
}

fn denied(record: &ConnectionRecord, reason: DenyReason) -> Outcome {
    Outcome::Denied {
        connection: record.name.clone(),
        reason,
    }
}

/// 按 id 或名称找一个连接。
///
/// 名称不是唯一键（数据库没做唯一约束），重名时**不猜**，把候选列出来让调用方换用 id。
async fn resolve_record(ctx: &Context, spec: &str) -> Result<ConnectionRecord, RpcError> {
    let rows = load_connections(ctx)
        .await
        .map_err(|err| RpcError::new(INTERNAL_ERROR, describe(&err)))?;

    if let Some(found) = rows.iter().find(|record| record.id == spec) {
        return Ok(found.clone());
    }

    let lowered = spec.trim().to_lowercase();
    let matched: Vec<_> = rows
        .iter()
        .filter(|record| record.name.to_lowercase() == lowered)
        .collect();

    match matched.len() {
        1 => Ok(matched[0].clone()),
        0 => Err(RpcError::new(
            INVALID_PARAMS,
            format!("没有名为 {spec} 的连接（先用 list_connections 看看有哪些）"),
        )),
        _ => {
            let ids: Vec<&str> = matched.iter().map(|record| record.id.as_str()).collect();
            Err(RpcError::new(
                INVALID_PARAMS,
                format!("有多个连接叫 {spec}，请改用 id：{}", ids.join(", ")),
            ))
        }
    }
}

fn required_str(args: &Value, key: &str) -> Result<String, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("缺少参数 {key}")))
}

fn text(body: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": body.into() }],
        "isError": false
    })
}

fn failed(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

/// 后端失败时把"下一步该做什么"一起给出去。
fn backend_failure(err: &AppError) -> Value {
    let mut message = describe(err);
    if err.code == ErrCode::CryptoKeyUnavailable {
        message.push_str(
            "\n处理方式：让用户打开一次 TermHarbor 应用（正常启动即可，不用点任何东西），\
             应用会自动准备后台可读的密钥；然后重启 MCP 宿主。\
             如果宿主是以另一个 Windows 账号运行的，需要改成用同一账号运行。\
             不要让用户手动导出密钥，也不要让用户去界面里点按钮。",
        );
    }
    failed(message)
}

/// 把 `AppError` 变成模型能读的一句话。
fn describe(err: &AppError) -> String {
    match err.detail.as_deref().filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("{}（{}）：{detail}", err.message, err.code.as_str()),
        None => format!("{}（{}）", err.message, err.code.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_the_four_whitelisted_tools() {
        let names: Vec<&str> = list().iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            ["list_connections", "sftp_list", "load_snapshot", "exec"]
        );
    }

    /// 红线：写操作、读文件内容、任意 URL 都不提供。
    #[test]
    fn no_write_or_read_file_tools() {
        for tool in list() {
            let name = tool.name;
            assert!(
                !name.contains("write")
                    && !name.contains("upload")
                    && !name.contains("remove")
                    && name != "read_file"
                    && name != "open_url",
                "{name} 不该出现"
            );
        }
    }

    #[test]
    fn every_tool_has_object_schema_with_properties() {
        for tool in list() {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            assert!(tool.input_schema.get("properties").is_some());
            assert!(!tool.description.is_empty());
        }
    }

    #[test]
    fn tools_that_take_args_declare_them_required() {
        let tools = list();
        let sftp = tools.iter().find(|t| t.name == "sftp_list").unwrap();
        let required = sftp.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::from("connection")));
        assert!(required.contains(&Value::from("path")));

        let exec = tools.iter().find(|t| t.name == "exec").unwrap();
        let required = exec.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::from("connection")));
        assert!(required.contains(&Value::from("command")));
    }

    #[test]
    fn required_str_rejects_missing_and_blank() {
        let args = json!({ "connection": "  ", "path": "/tmp" });
        assert!(required_str(&args, "connection").is_err());
        assert!(required_str(&args, "missing").is_err());
        assert_eq!(required_str(&args, "path").unwrap(), "/tmp");
    }

    #[test]
    fn text_result_is_not_an_error() {
        let value = text("ok");
        assert_eq!(value["isError"], false);
        assert_eq!(value["content"][0]["text"], "ok");
    }

    #[test]
    fn key_unavailable_error_tells_the_agent_exactly_what_to_do() {
        let err = AppError::new(ErrCode::CryptoKeyUnavailable);
        let value = backend_failure(&err);
        let body = value["content"][0]["text"].as_str().unwrap();
        assert_eq!(value["isError"], true);
        assert!(body.contains("CRYPTO_KEY_UNAVAILABLE"));
        assert!(body.contains("打开一次 TermHarbor"));
        assert!(body.contains("不要让用户去界面里点按钮"));
    }

    #[test]
    fn connection_list_description_rejects_online_assumptions() {
        let tool = list()
            .into_iter()
            .find(|t| t.name == "list_connections")
            .unwrap();
        assert!(tool.description.contains("不检测是否在线"));
    }

    #[test]
    fn describe_includes_code_and_detail() {
        let err = AppError::with_detail(ErrCode::SftpNotFound, "目录不存在");
        let line = describe(&err);
        assert!(line.contains("SFTP_NOT_FOUND"));
        assert!(line.contains("目录不存在"));
    }
}
