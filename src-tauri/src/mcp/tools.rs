//! 工具清单与实现。
//!
//! M8-1 只做**只读**三件事：看连接列表、列远端目录、看负载快照。
//! `read_file` 虽然也是只读，但"能读服务器上任意文件"等于把内容外泄面放开了，
//! 单独立项再决定（开发计划 §17.2）。
//!
//! 错误分两层，别混：
//! - **协议错误**（`RpcError`）：参数不对、工具名不认识 —— 调用方写错了。
//! - **执行失败**（`isError: true` 的 content）：连不上、没权限、目录不存在 ——
//!   这是要给模型看的内容，让它自己决定换个路径或者告诉用户。

use serde::Serialize;
use serde_json::{json, Value};

use super::context::Context;
use super::jsonrpc::{RpcError, INTERNAL_ERROR, INVALID_PARAMS};
use crate::error::{AppError, ErrCode};
use crate::monitor::load::LoadSnapshot;

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
            name: "list_connections",
            description: "列出 TermHarbor 里保存的服务器连接。只返回名称、主机、端口、用户名、\
                          认证方式，不含任何密码、私钥或主机指纹。\
                          这只是「已保存的连接」，不检测是否在线、也不代表能登录成功。",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: "sftp_list",
            description: "列出某台服务器上某个目录的内容。只读，不会修改任何文件。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": {
                        "type": "string",
                        "description": "连接名称或 id（来自 list_connections）"
                    },
                    "path": {
                        "type": "string",
                        "description": "远端绝对路径，例如 /var/log"
                    }
                },
                "required": ["connection", "path"],
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: "load_snapshot",
            description: "取某台服务器的负载快照（CPU、内存、磁盘、网络、进程数）。只读。",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "connection": {
                        "type": "string",
                        "description": "连接名称或 id（来自 list_connections）"
                    }
                },
                "required": ["connection"],
                "additionalProperties": false
            }),
        },
    ]
}

/// 分发一次工具调用。
pub async fn call(ctx: &Context, name: &str, arguments: Option<&Value>) -> Result<Value, RpcError> {
    match name {
        "list_connections" => list_connections(ctx).await,
        "sftp_list" => sftp_list(ctx, arguments).await,
        "load_snapshot" => load_snapshot(ctx, arguments).await,
        other => {
            let available: Vec<&str> = list().iter().map(|tool| tool.name).collect();
            Err(RpcError::new(
                INVALID_PARAMS,
                format!("未知工具 {other}；可用工具：{}", available.join(", ")),
            ))
        }
    }
}

async fn list_connections(ctx: &Context) -> Result<Value, RpcError> {
    // 后端错误一律走 `isError` 的 content，不用协议错误 —— 那是"你调错了"的意思，
    // 而"读库失败/连不上服务器"是执行结果，模型应该看到并据此改口。
    match load_connections(ctx).await {
        Ok(rows) => {
            // 只挑能给的字段。ConnectionRecord 里带着密文和私钥路径 ——
            // 直接序列化整条记录就等于把凭据结构暴露给模型了。
            let items: Vec<Value> = rows
                .iter()
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

            Ok(text(
                serde_json::to_string_pretty(&items).unwrap_or_default(),
            ))
        }
        Err(err) => Ok(backend_failure(&err)),
    }
}

async fn load_connections(
    ctx: &Context,
) -> crate::error::AppResult<Vec<crate::storage::connections::ConnectionRecord>> {
    ctx.db()?
        .call(|conn| crate::storage::connections::list(conn, None))
        .await
}

async fn sftp_list(ctx: &Context, arguments: Option<&Value>) -> Result<Value, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let path = required_str(&args, "path")?;

    if !path.starts_with('/') {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("path 必须是远端绝对路径（以 / 开头），收到：{path}"),
        ));
    }

    let id = resolve_connection_id(ctx, &spec).await?;

    match list_remote(ctx, &id, &path).await {
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

            Ok(text(format!(
                "{header}\n\n{}",
                serde_json::to_string_pretty(&items).unwrap_or_default()
            )))
        }
        Err(err) => Ok(backend_failure(&err)),
    }
}

/// 真连一次服务器、列一个目录、然后断开。
async fn list_remote(
    ctx: &Context,
    id: &str,
    path: &str,
) -> crate::error::AppResult<Vec<crate::sftp::RemoteEntry>> {
    let state = ctx.app_state()?;

    // 复用 GUI 的同一套策略：凭据解密、known_hosts 校验、老服务器兼容开关
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;

    // 一次调用一条连接，用完即关：MCP 侧没有会话池，也不该长期占着别人的 sshd
    let (_opened, conn) =
        crate::sftp::session::open(ctx.next_session_id(), prepared.request).await?;

    let rows = crate::sftp::list::read_dir(&conn.session, path).await;
    conn.close().await;

    rows
}

/// 采一次负载快照。走 `monitor` 里同一条采样命令，所以数字和界面上的看板一致。
async fn load_snapshot(ctx: &Context, arguments: Option<&Value>) -> Result<Value, RpcError> {
    let args = arguments.cloned().unwrap_or_else(|| json!({}));
    let spec = required_str(&args, "connection")?;
    let id = resolve_connection_id(ctx, &spec).await?;

    match sample_load(ctx, &id).await {
        Ok(snapshot) => Ok(text(
            serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
        )),
        Err(err) => Ok(backend_failure(&err)),
    }
}

async fn sample_load(ctx: &Context, id: &str) -> crate::error::AppResult<LoadSnapshot> {
    let state = ctx.app_state()?;
    let prepared =
        crate::commands::connections::prepare_probe(&state, Some(id.to_string()), None, None)
            .await?;

    // 一条 exec 通道跑完整条采样命令；内部自己 sleep 采两帧算速率
    let raw = crate::ssh::exec::exec_on_fresh_connection(
        prepared.request,
        crate::monitor::load::SAMPLE_COMMAND,
    )
    .await?;

    Ok(crate::monitor::load::parse(&raw))
}

/// 按 id 或名称找一个连接。
///
/// 名称不是唯一键（数据库没做唯一约束），重名时**不猜**，把候选列出来让调用方换用 id。
async fn resolve_connection_id(ctx: &Context, spec: &str) -> Result<String, RpcError> {
    let rows = load_connections(ctx)
        .await
        .map_err(|err| RpcError::new(INTERNAL_ERROR, describe(&err)))?;

    if let Some(found) = rows.iter().find(|record| record.id == spec) {
        return Ok(found.id.clone());
    }

    let lowered = spec.trim().to_lowercase();
    let matched: Vec<_> = rows
        .iter()
        .filter(|record| record.name.to_lowercase() == lowered)
        .collect();

    match matched.len() {
        1 => Ok(matched[0].id.clone()),
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

/// 工具自己执行失败：仍走正常回包，但标 `isError`，并说清是什么错。
fn failed(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

/// 后端失败时把"下一步该做什么"一起给出去。
///
/// 模型看到一句光秃秃的错误码会自己编解决办法（实测它会让用户"打开界面点一下"，
/// 甚至自己扫端口声称服务器在线）。密钥不可用这种有确定解法的问题，就该把解法写死在结果里。
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
///
/// 带上错误码：中文文案可能太笼统，错误码和界面上显示的是同一套，能直接对上。
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
    fn exposes_exactly_the_three_readonly_tools() {
        let names: Vec<&str> = list().iter().map(|tool| tool.name).collect();
        assert_eq!(names, ["list_connections", "sftp_list", "load_snapshot"]);
    }

    /// 红线：M8-1 不允许出现任何写操作或"读文件"工具。
    #[test]
    fn no_write_or_read_file_tools() {
        for tool in list() {
            let name = tool.name;
            assert!(
                !name.contains("write")
                    && !name.contains("upload")
                    && !name.contains("remove")
                    && !name.contains("exec")
                    && name != "read_file",
                "{name} 属于 M8-2/3，不该在只读档里出现"
            );
        }
    }

    #[test]
    fn every_tool_has_object_schema_with_properties() {
        for tool in list() {
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
            assert!(
                tool.input_schema.get("properties").is_some(),
                "{} 缺 properties",
                tool.name
            );
            assert!(
                !tool.description.is_empty(),
                "{} 没有描述，模型不知道什么时候该用它",
                tool.name
            );
        }
    }

    #[test]
    fn tools_that_take_args_declare_them_required() {
        let tools = list();
        let sftp = tools.iter().find(|t| t.name == "sftp_list").unwrap();
        let required = sftp.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::from("connection")));
        assert!(required.contains(&Value::from("path")));

        let list_conn = tools.iter().find(|t| t.name == "list_connections").unwrap();
        assert_eq!(list_conn.input_schema["additionalProperties"], false);
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
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "ok");
    }

    /// 密钥不可用时必须给出确定的解法，别让模型自己编（实测它会让用户去点 GUI）。
    #[test]
    fn key_unavailable_error_tells_the_agent_exactly_what_to_do() {
        let err = AppError::new(ErrCode::CryptoKeyUnavailable);
        let value = backend_failure(&err);
        let text = value["content"][0]["text"].as_str().unwrap();

        assert_eq!(value["isError"], true);
        assert!(text.contains("CRYPTO_KEY_UNAVAILABLE"));
        assert!(
            text.contains("打开一次 TermHarbor"),
            "要让用户打开一次应用：{text}"
        );
        assert!(text.contains("重启 MCP 宿主"), "要说清重启宿主：{text}");
        assert!(
            text.contains("不要让用户去界面里点按钮"),
            "要明确禁止它自作主张：{text}"
        );
    }

    /// 连接列表不是探活结果，说明里必须写死，否则模型会自己扫端口编一个"在线"出来。
    #[test]
    fn connection_list_description_rejects_online_assumptions() {
        let tool = list()
            .into_iter()
            .find(|t| t.name == "list_connections")
            .unwrap();
        assert!(
            tool.description.contains("不检测是否在线"),
            "{}",
            tool.description
        );
    }

    #[test]
    fn describe_includes_code_and_detail() {
        let err = AppError::with_detail(crate::error::ErrCode::SftpNotFound, "目录不存在");
        let line = describe(&err);
        assert!(line.contains("SFTP_NOT_FOUND"));
        assert!(line.contains("目录不存在"));
    }
}
