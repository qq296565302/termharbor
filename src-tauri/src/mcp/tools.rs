//! 工具清单与实现。
//!
//! 闸门全部在**服务端**：连接是否启用 → 工具是否允许 → 参数（路径/命令）→ 执行限额。
//! agent 传什么参数都绕不过去，因为它不参与任何放行判断。
//!
//! 0.1.28 起「启用」就是这台机器的全部能力，三个列表留空表示不限；
//! 唯一始终不让步的是**复合命令**（`;`、管道、重定向等），因为它破坏的是
//! 「一次调用只做一件事」这个契约，而不是权限 —— agent 要多步就多调几次。
//!
//! 错误分三层，别混：
//! - **协议错误**（`RpcError`）：参数不对、工具名不认识 —— 调用方写错了。
//! - **拒绝**（`Outcome::Denied`）：配置把这条收窄掉了 —— 明确告诉模型"找用户开"。
//! - **执行失败**（`isError` 的 content）：连不上、没权限、目录不存在 —— 让模型据此改口。

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::config::{
    ConnectionPolicy, ExecPolicy, McpConfig, ALL_TOOLS, TOOL_EXEC, TOOL_LIST_CONNECTIONS,
    TOOL_LOAD_SNAPSHOT, TOOL_SFTP_DOWNLOAD, TOOL_SFTP_LIST, TOOL_SFTP_MKDIR, TOOL_SFTP_READ,
    TOOL_SFTP_RENAME, TOOL_SFTP_UPLOAD, TOOL_SFTP_WRITE,
};
use super::context::Context;
use super::exec::{validate_command, DenyReason};
use super::jsonrpc::{RpcError, INTERNAL_ERROR, INVALID_PARAMS};
use crate::error::{AppError, ErrCode};
use crate::monitor::load::LoadSnapshot;
use crate::redact::{redact, redact_command};
use crate::state::now_millis;
use crate::storage::connections::ConnectionRecord;

/// 一次最多返回多少条目录项。
///
/// 远端有个 5 万文件的目录时，全塞给模型既没用又贵。超出部分明确告诉它"被截断了"，
/// 比让它以为目录就这么点内容安全。
const MAX_ENTRIES: usize = 200;

/// 读文件的默认与硬顶字节数。
///
/// 这不是安全边界，是**上下文预算**：1MB 文本进模型要花掉几万 token，
/// 还没开始干活就把窗口撑爆了。要整份文件就让 agent 走 `sftp_download`。
const DEFAULT_READ_BYTES: u64 = 256 * 1024;
const MAX_READ_BYTES: u64 = 1024 * 1024;

/// 直接写入的正文上限（`sftp_write` 的 `content`）。
const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// 审计里参数摘要的字符上限。够定位一次调用，又不至于把日志写垮。
const AUDIT_ARG_CHARS: usize = 512;

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
            description: "列出服务器上某个目录的内容。",
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
            name: TOOL_SFTP_READ,
            description: "读服务器上的文本文件。返回文件内容。\
                          只能读文本（含 NUL 字节的文件会被拒绝，请改用 sftp_download）；\
                          默认最多读 256KB，更大的文件也会被拒绝而不是截断。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "path": { "type": "string", "description": "远端绝对路径" },
                    "maxBytes": { "type": "integer", "description": "最多读多少字节，默认 262144，上限 1048576" }
                },
                "required": ["connection", "path"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_WRITE,
            description: "把文本内容写入服务器上的文件（覆盖写，父目录需已存在）。\
                          单次最多 1MB；更大的内容请用 sftp_upload。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "path": { "type": "string", "description": "远端绝对路径" },
                    "content": { "type": "string", "description": "要写入的完整文本" }
                },
                "required": ["connection", "path", "content"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_UPLOAD,
            description: "把本机文件上传到服务器。文件较大时会等传输完成才返回。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "localPath": { "type": "string", "description": "本机绝对路径" },
                    "remotePath": { "type": "string", "description": "远端绝对路径（含文件名）" }
                },
                "required": ["connection", "localPath", "remotePath"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_DOWNLOAD,
            description: "把服务器上的文件下载到本机。目标父目录必须已存在。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "remotePath": { "type": "string", "description": "远端绝对路径" },
                    "localPath": { "type": "string", "description": "本机绝对路径；若指向已存在的目录则沿用远端文件名" }
                },
                "required": ["connection", "remotePath", "localPath"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_MKDIR,
            description: "在服务器上新建目录（单层，父目录需已存在）。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "path": { "type": "string", "description": "远端绝对路径" }
                },
                "required": ["connection", "path"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_SFTP_RENAME,
            description: "在服务器上重命名或移动文件/目录。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "连接名称或 id" },
                    "from": { "type": "string", "description": "远端源绝对路径" },
                    "to": { "type": "string", "description": "远端目标绝对路径" }
                },
                "required": ["connection", "from", "to"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: TOOL_LOAD_SNAPSHOT,
            description: "取服务器的负载快照（CPU、内存、磁盘、网络、进程数）。只读。",
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
            description: "在服务器上执行一条命令。默认允许任意单条命令；\
                          复合命令（;、管道、重定向、$() 等）一律拒绝，需要多步就分多次调用。\
                          如果用户配了命令白名单，则只能执行白名单里的命令。",
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
        // exec 记命令原文（设置页「允许这条」要用），但 `mysql -pXXX` 这种
        // 短选项密码 redact 认不出来，得整段交给 redact_command 走一遍。
        arguments
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .map(redact_command)
            .unwrap_or_default()
    } else {
        // sftp_write 的 content 可能有 1MB，原样落盘会把 audit.jsonl 撑爆；
        // 正文本来也不该进审计，留个能定位的摘要就够
        let raw = arguments.map(ToString::to_string).unwrap_or_default();
        truncate_for_audit(&raw, AUDIT_ARG_CHARS)
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
        TOOL_SFTP_READ => sftp_read(ctx, arguments).await,
        TOOL_SFTP_WRITE => sftp_write(ctx, arguments).await,
        TOOL_SFTP_UPLOAD => sftp_upload(ctx, arguments).await,
        TOOL_SFTP_DOWNLOAD => sftp_download(ctx, arguments).await,
        TOOL_SFTP_MKDIR => sftp_mkdir(ctx, arguments).await,
        TOOL_SFTP_RENAME => sftp_rename(ctx, arguments).await,
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
        Outcome::Denied {
            reason: DenyReason::SecretPath,
            ..
        } => Ok(failed(
            "这个路径是凭据类文件（SSH 私钥、authorized_keys、/etc/shadow、云厂商凭据等），\
             本地策略默认不通过 MCP 读写。这是刻意的保护，不是权限没开。\
             如果确实需要，请让用户在「设置 → MCP 接入」里展开这台机器，\
             勾选「允许访问凭据类文件」；在此之前请换别的办法，不要重复尝试同一个路径。",
        )),
        Outcome::Denied { reason, .. } => Ok(failed(format!(
            "已被本地配置拒绝（{}）。这台机器或这个工具没有对 MCP 开放。\
             请让用户在「设置 → MCP 接入」里放开；不要尝试改用别的方式达到同样目的。",
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

/// 单个远端路径的闸门：先看 `paths` 收窄，再看凭据类文件。
///
/// 两道判定的意图不同，所以顺序也有讲究：`paths` 是用户自己划的范围，
/// 撞上了是「你没开这个目录」；凭据文件是**默认就挡**的，撞上了是
/// 「这个东西会放开更大的权限，要单独点一下」。
fn path_gate(policy: &ConnectionPolicy, path: &str) -> Result<(), DenyReason> {
    if !McpConfig::allows_path(policy, path) {
        return Err(DenyReason::PathNotAllowed);
    }
    if !policy.allow_secret_files && crate::mcp::config::secret_path_reason(path).is_some() {
        return Err(DenyReason::SecretPath);
    }
    Ok(())
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
    ensure_remote_absolute(&path)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_LIST) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &path) {
        return Ok(denied(&record, reason));
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
    let conn = open_sftp(ctx, id).await?;
    let rows = crate::sftp::list::read_dir(&conn.session, path).await;
    conn.close().await;
    rows
}

/// 单开一条 SFTP 连接，用完由调用方关。
///
/// 不复用 GUI 的会话表：MCP 是独立进程，那边的 sessionId 根本拿不到；
/// 而且每次调用都是短命操作，挂着一条连接只会让服务器上多一个没人用的 sshd。
async fn open_sftp(
    ctx: &Context,
    id: &str,
) -> crate::error::AppResult<std::sync::Arc<crate::sftp::SftpConn>> {
    let state = ctx.app_state()?;
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;
    let (_opened, conn) =
        crate::sftp::session::open(ctx.next_session_id(), prepared.request).await?;
    Ok(conn)
}

fn ensure_remote_absolute(path: &str) -> Result<(), RpcError> {
    if !path.starts_with('/') {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("path 必须是远端绝对路径（以 / 开头），收到：{path}"),
        ));
    }
    Ok(())
}

/// 读远端文本文件。
async fn sftp_read(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let path = required_str(&args, "path")?;
    ensure_remote_absolute(&path)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_READ) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &path) {
        return Ok(denied(&record, reason));
    }

    let limit = args
        .get("maxBytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_BYTES)
        .clamp(1, MAX_READ_BYTES);

    match read_remote(ctx, &record.id, &path, limit).await {
        Ok((body, size)) => {
            let header = format!("{path}（{size} 字节，{} 行）", body.lines().count().max(1));
            Ok(Outcome::Ok {
                value: text(format!("{header}\n\n{body}")),
                connection: record.name,
            })
        }
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn read_remote(
    ctx: &Context,
    id: &str,
    path: &str,
    limit: u64,
) -> crate::error::AppResult<(String, u64)> {
    let conn = open_sftp(ctx, id).await?;

    let result = async {
        let Some((is_dir, size)) = crate::sftp::ops::stat(&conn.session, path).await? else {
            return Err(AppError::with_detail(
                ErrCode::SftpNotFound,
                format!("远端没有这个路径: {path}"),
            ));
        };
        if is_dir {
            return Err(AppError::validation(format!(
                "{path} 是目录，列目录请用 sftp_list"
            )));
        }
        if size > limit {
            return Err(AppError::validation(format!(
                "{path} 有 {size} 字节，超过本次读取上限 {limit}。\
                 要整份内容请用 sftp_download，或调大 maxBytes（硬上限 {MAX_READ_BYTES}）"
            )));
        }

        let mut file = conn
            .session
            .open(path)
            .await
            .map_err(crate::sftp::map_error)?;
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf).await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("读文件失败: {err}"))
        })?;

        // NUL 字节是二进制文件的可靠特征。直接当文本返回会被模型当成乱码，
        // 既浪费上下文又办不成事，不如明确让它换 sftp_download。
        if buf.contains(&0u8) {
            return Err(AppError::validation(format!(
                "{path} 看起来是二进制文件，请用 sftp_download 取回本机处理"
            )));
        }
        let body = String::from_utf8(buf).map_err(|_| {
            AppError::validation(format!(
                "{path} 不是有效 UTF-8 文本，请用 sftp_download 取回本机处理"
            ))
        })?;
        Ok((body, size))
    }
    .await;

    conn.close().await;
    result
}

/// 覆盖写远端文件。
async fn sftp_write(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let path = required_str(&args, "path")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "缺少参数 content"))?
        .to_string();
    ensure_remote_absolute(&path)?;

    if content.len() > MAX_WRITE_BYTES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!(
                "content 有 {} 字节，超过单次写入上限 {MAX_WRITE_BYTES}；大文件请用 sftp_upload",
                content.len()
            ),
        ));
    }

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_WRITE) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &path) {
        return Ok(denied(&record, reason));
    }

    match write_remote(ctx, &record.id, &path, &content).await {
        Ok(bytes) => Ok(Outcome::Ok {
            value: text(format!("已写入 {path}，{bytes} 字节")),
            connection: record.name,
        }),
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn write_remote(
    ctx: &Context,
    id: &str,
    path: &str,
    content: &str,
) -> crate::error::AppResult<u64> {
    let conn = open_sftp(ctx, id).await?;

    let result = async {
        let mut file = conn
            .session
            .create(path)
            .await
            .map_err(crate::sftp::map_error)?;
        file.write_all(content.as_bytes()).await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("写文件失败: {err}"))
        })?;
        file.close().await.map_err(|err| {
            AppError::with_detail(ErrCode::SftpIoError, format!("关闭远端文件失败: {err}"))
        })?;
        Ok(content.len() as u64)
    }
    .await;

    conn.close().await;
    result
}

/// 本机 → 远端。
async fn sftp_upload(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let local_path = required_str(&args, "localPath")?;
    let remote_path = required_str(&args, "remotePath")?;
    ensure_remote_absolute(&remote_path)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_UPLOAD) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &remote_path) {
        return Ok(denied(&record, reason));
    }

    let local = std::path::PathBuf::from(&local_path);
    let state = ctx.app_state().map_err(internal)?;
    // 应用自己的数据目录里有加密凭据和主机指纹，绝不能被当成上传源。
    let protected: [&std::path::Path; 2] = [&state.paths.data_dir, &state.paths.backup_dir];
    crate::sftp::transfer::ensure_upload_source(&local, &protected).map_err(internal)?;

    match upload_file(ctx, &record.id, &local, &remote_path).await {
        Ok(bytes) => Ok(Outcome::Ok {
            value: text(format!("已上传 {local_path} → {remote_path}，{bytes} 字节")),
            connection: record.name,
        }),
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn upload_file(
    ctx: &Context,
    id: &str,
    local: &std::path::Path,
    remote_path: &str,
) -> crate::error::AppResult<u64> {
    let conn = open_sftp(ctx, id).await?;
    let state = ctx.app_state()?;
    let protected: [&std::path::Path; 2] = [&state.paths.data_dir, &state.paths.backup_dir];
    let emit: crate::sftp::transfer::ProgressFn = Box::new(|_| {});

    let result = crate::sftp::transfer::upload(
        &conn.session,
        local,
        remote_path,
        ctx.next_session_id(),
        tokio_util::sync::CancellationToken::new(),
        emit,
        &protected,
    )
    .await;

    conn.close().await;
    result
}

/// 远端 → 本机。
async fn sftp_download(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let remote_path = required_str(&args, "remotePath")?;
    let local_path = required_str(&args, "localPath")?;
    ensure_remote_absolute(&remote_path)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_DOWNLOAD) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &remote_path) {
        return Ok(denied(&record, reason));
    }

    let state = ctx.app_state().map_err(internal)?;
    let mut local = std::path::PathBuf::from(&local_path);
    // 给一个已存在的目录时沿用远端文件名，省得调用方每次都自己拼
    if local.is_dir() {
        let name = remote_path.rsplit('/').next().unwrap_or_default();
        if name.is_empty() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "remotePath 缺少文件名，无法确定本地落盘名称",
            ));
        }
        local = local.join(name);
    }
    // localPath 是调用方给的，不能假设它来自文件对话框
    if local.starts_with(&state.paths.data_dir) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "不允许把下载目标放进应用数据目录",
        ));
    }

    match download_file(ctx, &record.id, &remote_path, &local).await {
        Ok(bytes) => Ok(Outcome::Ok {
            value: text(format!(
                "已下载 {remote_path} → {}，{bytes} 字节",
                local.display()
            )),
            connection: record.name,
        }),
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

async fn download_file(
    ctx: &Context,
    id: &str,
    remote_path: &str,
    local: &std::path::Path,
) -> crate::error::AppResult<u64> {
    let conn = open_sftp(ctx, id).await?;
    let emit: crate::sftp::transfer::ProgressFn = Box::new(|_| {});

    let result = crate::sftp::transfer::download(
        &conn.session,
        remote_path,
        local,
        ctx.next_session_id(),
        tokio_util::sync::CancellationToken::new(),
        emit,
    )
    .await;

    conn.close().await;
    result
}

/// 新建目录。
async fn sftp_mkdir(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let path = required_str(&args, "path")?;
    ensure_remote_absolute(&path)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_MKDIR) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    if let Err(reason) = path_gate(&policy, &path) {
        return Ok(denied(&record, reason));
    }

    match open_sftp(ctx, &record.id).await {
        Ok(conn) => {
            let result = crate::sftp::ops::create_dir(&conn.session, &path).await;
            conn.close().await;
            match result {
                Ok(()) => Ok(Outcome::Ok {
                    value: text(format!("已创建目录 {path}")),
                    connection: record.name,
                }),
                Err(err) => Ok(Outcome::Error {
                    connection: record.name,
                    err,
                }),
            }
        }
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
}

/// 重命名 / 移动。
async fn sftp_rename(ctx: &Context, arguments: Option<&Value>) -> Result<Outcome, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let from = required_str(&args, "from")?;
    let to = required_str(&args, "to")?;
    ensure_remote_absolute(&from)?;
    ensure_remote_absolute(&to)?;

    let record = resolve_record(ctx, &spec).await?;
    let cfg = ctx.config().load();
    let policy = match gated(&cfg, &record, TOOL_SFTP_RENAME) {
        Ok(policy) => policy,
        Err(reason) => return Ok(denied(&record, reason)),
    };
    // 两头都要过：只校验 from 的话，`rename("/tmp/a", "/etc/passwd")` 就绕过去了；
    // `rename("/home/u/.ssh/id_rsa", "/tmp/x")` 这种平移也一样要拦。
    if let Err(reason) = path_gate(&policy, &from).and_then(|()| path_gate(&policy, &to)) {
        return Ok(denied(&record, reason));
    }

    match open_sftp(ctx, &record.id).await {
        Ok(conn) => {
            let result = crate::sftp::ops::rename(&conn.session, &from, &to).await;
            conn.close().await;
            match result {
                Ok(()) => Ok(Outcome::Ok {
                    value: text(format!("已重命名 {from} → {to}")),
                    connection: record.name,
                }),
                Err(err) => Ok(Outcome::Error {
                    connection: record.name,
                    err,
                }),
            }
        }
        Err(err) => Ok(Outcome::Error {
            connection: record.name,
            err,
        }),
    }
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
    policy: &ExecPolicy,
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

/// 本地环境问题（拿不到密钥、库打不开）不该伪装成「参数错了」。
fn internal(err: AppError) -> RpcError {
    RpcError::new(INTERNAL_ERROR, describe(&err))
}

/// 按**字符**边界截断，不能按字节切 —— 切在 UTF-8 中间会 panic。
fn truncate_for_audit(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_string();
    }
    let head: String = raw.chars().take(limit).collect();
    format!("{head}…（共 {} 字符）", raw.chars().count())
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

    fn open_policy() -> ConnectionPolicy {
        ConnectionPolicy {
            enabled: true,
            ..Default::default()
        }
    }

    /// 凭据类文件默认挡：不限路径 ≠ 什么都能读。
    #[test]
    fn secret_paths_are_blocked_even_without_path_narrowing() {
        let policy = open_policy();
        for path in [
            "/root/.ssh/id_rsa",
            "/home/deploy/.ssh/authorized_keys",
            "/etc/shadow",
            "/home/a/.aws/credentials",
        ] {
            assert_eq!(
                path_gate(&policy, path),
                Err(DenyReason::SecretPath),
                "该挡住：{path}"
            );
        }
    }

    /// 普通文件不能挡，否则连日常排查都做不了。
    #[test]
    fn ordinary_paths_pass_the_gate() {
        let policy = open_policy();
        for path in [
            "/var/log/nginx/error.log",
            "/etc/nginx/nginx.conf",
            "/home/a/.ssh/config",
            "/opt/app/.env",
        ] {
            assert_eq!(path_gate(&policy, path), Ok(()), "不该挡：{path}");
        }
    }

    /// 勾了「允许凭据类文件」就真放开 —— 不然这个开关没意义。
    #[test]
    fn allow_secret_files_opens_the_gate() {
        let policy = ConnectionPolicy {
            enabled: true,
            allow_secret_files: true,
            ..Default::default()
        };
        assert_eq!(path_gate(&policy, "/root/.ssh/id_rsa"), Ok(()));
        assert_eq!(path_gate(&policy, "/etc/shadow"), Ok(()));
    }

    /// 路径收窄优先于凭据判定：自己划的范围外，连报错都不该透露文件属于哪类。
    #[test]
    fn path_narrowing_wins_over_secret_check() {
        let policy = ConnectionPolicy {
            enabled: true,
            paths: vec!["/var/log".into()],
            ..Default::default()
        };
        assert_eq!(
            path_gate(&policy, "/root/.ssh/id_rsa"),
            Err(DenyReason::PathNotAllowed)
        );
    }

    #[test]
    fn exposes_every_declared_tool_in_stable_order() {
        let names: Vec<&str> = list().iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            ALL_TOOLS.to_vec(),
            "清单顺序即声明顺序，宿主端靠它做 diff"
        );
    }

    /// 文件类工具必须都挂在 ALL_TOOLS 上，否则配置校验会认为它们非法。
    #[test]
    fn file_tools_are_all_declared() {
        for tool in list() {
            assert!(
                ALL_TOOLS.contains(&tool.name),
                "{} 不在 ALL_TOOLS 里，保存配置时会被判成未知工具",
                tool.name
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
    fn audit_args_are_truncated_without_breaking_utf8() {
        let long = "日志".repeat(1000);
        let cut = truncate_for_audit(&long, 512);
        assert!(cut.chars().count() < 530, "应当被截断");
        assert!(cut.ends_with("字符）"), "要说明被截断了");

        let short = "df -h";
        assert_eq!(truncate_for_audit(short, 512), short, "短参数原样保留");
    }

    #[test]
    fn describe_includes_code_and_detail() {
        let err = AppError::with_detail(ErrCode::SftpNotFound, "目录不存在");
        let line = describe(&err);
        assert!(line.contains("SFTP_NOT_FOUND"));
        assert!(line.contains("目录不存在"));
    }
}
