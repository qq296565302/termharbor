//! 一次一行请求的分发器。
//!
//! 与 IO 完全解耦：`handle_line` 吃一行字符串、吐一行字符串（通知返回 `None`），
//! 这样整条协议链路都能用单测覆盖，不需要真的起进程。

use serde_json::{json, Value};

use super::context::Context;
use super::jsonrpc::{Request, Response, INVALID_PARAMS, METHOD_NOT_FOUND};
use super::tools;

/// 支持的 MCP 协议版本，新的在前。
///
/// 宿主会带一个版本号来；认识就原样回，不认识就回我们最新的那个 —— 规范里说
/// 客户端收到不支持的版本应当断开，所以这里不能瞎编一个字符串。
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// 给模型看的开场白。写得越具体，模型越少乱调工具。
const INSTRUCTIONS: &str = "TermHarbor 终端港：通过本机已保存的连接访问服务器，**只做只读操作**。\
先调用 list_connections 拿到连接名，再用 sftp_list / load_snapshot 查询。\
不会也不能返回密码、私钥或主机指纹。\
\
纪律（很重要）：\
1. 只用本服务的工具查询。不要启动图形界面、不要自己扫描端口、不要让用户去界面里手动点按钮。\
2. list_connections 只是「已保存的连接」，不表示在线，也不表示能登录成功；不要据此断言在线状态。\
3. 工具报错时，把错误原文和它给的处理建议原样转告用户并停下，不要自行改用别的方式重试。";

pub struct Server {
    ctx: Context,
    name: &'static str,
    version: &'static str,
}

impl Server {
    pub fn new(ctx: Context) -> Self {
        Self {
            ctx,
            name: "termharbor",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// 处理一行。返回 `None` 表示这是通知，不需要回包。
    pub async fn handle_line(&self, raw: &str) -> Option<String> {
        let line = raw.trim();
        if line.is_empty() {
            return None;
        }

        let request: Request = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => {
                return Some(Response::parse_failed(format!("请求不是合法 JSON: {err}")).to_line())
            }
        };

        // 通知：有 method 没 id。规范要求不回包 —— 回了反而会让宿主记一次协议错误。
        // 这里用 `?` 直接短路成 `None`，与"通知不产生回包"是同一件事。
        let id = request.id.clone()?;

        let response = match request.method.as_str() {
            "initialize" => Response::ok(id, self.initialize(request.params.as_ref())),
            "ping" => Response::ok(id, json!({})),
            "tools/list" => Response::ok(id, json!({ "tools": tools::list() })),
            "tools/call" => match self.call_tool(request.params.as_ref()).await {
                Ok(result) => Response::ok(id, result),
                Err(err) => Response::from_error(id, err),
            },
            other => Response::err(id, METHOD_NOT_FOUND, format!("不支持的方法: {other}")),
        };

        Some(response.to_line())
    }

    fn initialize(&self, params: Option<&Value>) -> Value {
        let requested = params
            .and_then(|value| value.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or(SUPPORTED_VERSIONS[0]);

        // 认识就顺着宿主，不认识就用我们最新的：宿主会据此判断要不要继续。
        let version = if SUPPORTED_VERSIONS.contains(&requested) {
            requested
        } else {
            SUPPORTED_VERSIONS[0]
        };

        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.name, "version": self.version },
            "instructions": INSTRUCTIONS
        })
    }

    async fn call_tool(&self, params: Option<&Value>) -> Result<Value, super::jsonrpc::RpcError> {
        let params = params.ok_or_else(|| {
            super::jsonrpc::RpcError::new(INVALID_PARAMS, "tools/call 需要 params")
        })?;

        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| super::jsonrpc::RpcError::new(INVALID_PARAMS, "缺少工具名 name"))?;

        tools::call(&self.ctx, name, params.get("arguments")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每次调用都要一个**独立的临时目录**。
    ///
    /// 早先按 pid 拼名字，同一进程里的测试并行跑时会互相删目录（`remove_dir_all`），
    /// 症状是"单跑过、一起跑挂"—— 和 ssh 测试那边踩过的是同一类坑。
    fn server() -> Server {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "termharbor-mcp-server-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        Server::new(Context::for_tests(dir))
    }

    async fn call(server: &Server, line: &str) -> Value {
        let out = server.handle_line(line).await.expect("这条该有回包");
        serde_json::from_str(&out).expect("回包必须是合法 JSON")
    }

    #[tokio::test]
    async fn initialize_echoes_supported_version() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        )
        .await;

        assert_eq!(value["id"], 1);
        assert_eq!(value["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(value["result"]["serverInfo"]["name"], "termharbor");
        assert_eq!(value["result"]["capabilities"]["tools"], json!({}));
        assert!(
            value["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("只读"),
            "开场白里要明确只读，减少模型乱调"
        );
    }

    /// agent 曾经自己扫端口、启动 GUI 让用户点按钮。开场白里把这三条纪律钉死。
    #[tokio::test]
    async fn instructions_forbid_gui_and_port_scanning() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        )
        .await;
        let text = value["result"]["instructions"].as_str().unwrap();

        assert!(text.contains("不要启动图形界面"), "{text}");
        assert!(text.contains("不要自己扫描端口"), "{text}");
        assert!(text.contains("不表示在线"), "{text}");
    }

    #[tokio::test]
    async fn unknown_protocol_version_falls_back_to_newest() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        )
        .await;
        assert_eq!(value["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        assert!(
            server()
                .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .await
                .is_none(),
            "通知不能回包"
        );
    }

    #[tokio::test]
    async fn tools_list_returns_three_tools() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        )
        .await;
        let tools = value["result"]["tools"]
            .as_array()
            .expect("要有 tools 数组");
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["name"], "list_connections");
        assert!(tools[0]["inputSchema"]["type"] == "object");
    }

    /// `list_connections` 是唯一一个不碰 SSH 的实现，端到端跑一遍：
    /// 空库应当回一个空数组，而且**不是**错误。
    #[tokio::test]
    async fn list_connections_on_empty_db_returns_empty_array() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}"#,
        )
        .await;

        assert_eq!(value["result"]["isError"], false, "空库不是错误：{value}");
        let body = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(body.trim(), "[]", "空库应回空数组，实际：{body}");
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_invalid_params() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
        )
        .await;
        assert_eq!(value["error"]["code"], INVALID_PARAMS);
        // 错误响应不能带 result（哪怕是 null）：规范要求二者互斥，严格的宿主会判协议错误
        assert!(
            value.get("result").is_none(),
            "错误响应里不该出现 result 字段，实际: {value}"
        );
    }

    /// 参数缺失属于"你调错了"，走协议错误，不是执行失败。
    #[tokio::test]
    async fn sftp_list_without_arguments_is_invalid_params() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"sftp_list","arguments":{"connection":"x"}}}"#,
        )
        .await;
        assert_eq!(value["error"]["code"], INVALID_PARAMS);
    }

    /// load_snapshot 要连服务器，没法在单测里跑通；但参数校验和"连接不存在"这两步
    /// 在本地就能覆盖 —— 它们是最容易被改坏的入口。
    #[tokio::test]
    async fn load_snapshot_rejects_missing_and_unknown_connection() {
        let server = server();

        let missing = call(
            &server,
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"load_snapshot","arguments":{}}}"#,
        )
        .await;
        assert_eq!(missing["error"]["code"], INVALID_PARAMS);

        let unknown = call(
            &server,
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"load_snapshot","arguments":{"connection":"没有这台"}}}"#,
        )
        .await;
        assert_eq!(unknown["error"]["code"], INVALID_PARAMS);
        assert!(
            unknown["error"]["message"]
                .as_str()
                .unwrap()
                .contains("list_connections"),
            "错误信息要指路：{unknown}"
        );
    }

    #[tokio::test]
    async fn tools_call_without_params_is_invalid_params() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call"}"#,
        )
        .await;
        assert_eq!(value["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":8,"method":"resources/list"}"#,
        )
        .await;
        assert_eq!(value["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn broken_json_reports_parse_error_with_null_id() {
        let value = call(&server(), "{not json").await;
        assert_eq!(value["error"]["code"], super::super::jsonrpc::PARSE_ERROR);
        assert!(value["id"].is_null());
    }

    #[tokio::test]
    async fn empty_input_is_ignored() {
        assert!(server().handle_line("   ").await.is_none());
    }

    #[tokio::test]
    async fn ping_replies_with_empty_object() {
        let value = call(&server(), r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#).await;
        assert_eq!(value["result"], json!({}));
    }

    /// 端到端：把"宿主会发的那几行"按顺序喂进去，回包必须都能往回解析。
    #[tokio::test]
    async fn full_handshake_then_list() {
        let server = server();
        let lines = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ];

        let mut replies = Vec::new();
        for line in lines {
            if let Some(out) = server.handle_line(line).await {
                replies.push(out);
            }
        }

        assert_eq!(replies.len(), 2, "通知不应产生回包");
        assert!(replies
            .iter()
            .all(|out| serde_json::from_str::<Value>(out).is_ok()));
    }
}
