//! 一次一行请求的分发器。
//!
//! 与 IO 完全解耦：`handle_line` 吃一行字符串、吐一行字符串（通知返回 `None`），
//! 这样整条协议链路都能用单测覆盖，不需要真的起进程。

use serde_json::{json, Value};

use super::jsonrpc::{INVALID_PARAMS, METHOD_NOT_FOUND, Request, Response};
use super::tools;

/// 支持的 MCP 协议版本，新的在前。
///
/// 宿主会带一个版本号来；认识就原样回，不认识就回我们最新的那个 —— 规范里说
/// 客户端收到不支持的版本应当断开，所以这里不能瞎编一个字符串。
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// 给模型看的开场白。写得越具体，模型越少乱调工具。
const INSTRUCTIONS: &str = "TermHarbor 终端港：通过本机已保存的连接访问服务器，**只做只读操作**。\
先调用 list_connections 拿到连接名，再用 sftp_list / load_snapshot 查询。\
不会也不能返回密码、私钥或主机指纹。";

pub struct Server {
    name: &'static str,
    version: &'static str,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Self {
            name: "termharbor",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// 处理一行。返回 `None` 表示这是通知，不需要回包。
    pub fn handle_line(&self, raw: &str) -> Option<String> {
        let line = raw.trim();
        if line.is_empty() {
            return None;
        }

        let request: Request = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => return Some(Response::parse_failed(format!("请求不是合法 JSON: {err}")).to_line()),
        };

        // 通知：有 method 没 id。规范要求不回包 —— 回了反而会让宿主记一次协议错误。
        // 这里用 `?` 直接短路成 `None`，与"通知不产生回包"是同一件事。
        let id = request.id.clone()?;

        let response = match request.method.as_str() {
            "initialize" => Response::ok(id, self.initialize(request.params.as_ref())),
            "ping" => Response::ok(id, json!({})),
            "tools/list" => Response::ok(id, json!({ "tools": tools::list() })),
            "tools/call" => match self.call_tool(request.params.as_ref()) {
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

    fn call_tool(&self, params: Option<&Value>) -> Result<Value, super::jsonrpc::RpcError> {
        let params = params.ok_or_else(|| {
            super::jsonrpc::RpcError::new(INVALID_PARAMS, "tools/call 需要 params")
        })?;

        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| super::jsonrpc::RpcError::new(INVALID_PARAMS, "缺少工具名 name"))?;

        tools::call(name, params.get("arguments"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &Server, line: &str) -> Value {
        let out = server.handle_line(line).expect("这条该有回包");
        serde_json::from_str(&out).expect("回包必须是合法 JSON")
    }

    #[test]
    fn initialize_echoes_supported_version() {
        let server = Server::new();
        let value = call(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        );

        assert_eq!(value["id"], 1);
        assert_eq!(value["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(value["result"]["serverInfo"]["name"], "termharbor");
        assert_eq!(value["result"]["capabilities"]["tools"], json!({}));
        assert!(
            value["result"]["instructions"].as_str().unwrap().contains("只读"),
            "开场白里要明确只读，减少模型乱调"
        );
    }

    #[test]
    fn unknown_protocol_version_falls_back_to_newest() {
        let server = Server::new();
        let value = call(
            &server,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(value["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);
    }

    #[test]
    fn notifications_get_no_response() {
        let server = Server::new();
        assert!(
            server
                .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none(),
            "通知不能回包"
        );
    }

    #[test]
    fn tools_list_returns_three_tools() {
        let server = Server::new();
        let value = call(&server, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        let tools = value["result"]["tools"].as_array().expect("要有 tools 数组");
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["name"], "list_connections");
        assert!(tools[0]["inputSchema"]["type"] == "object");
    }

    #[test]
    fn tools_call_known_tool_returns_content() {
        let server = Server::new();
        let value = call(
            &server,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}"#,
        );
        assert_eq!(value["result"]["isError"], true);
        assert_eq!(value["result"]["content"][0]["type"], "text");
    }

    #[test]
    fn tools_call_unknown_tool_is_invalid_params() {
        let server = Server::new();
        let value = call(
            &server,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
        );
        assert_eq!(value["error"]["code"], INVALID_PARAMS);
        // 错误响应不能带 result（哪怕是 null）：规范要求二者互斥，严格的宿主会判协议错误
        assert!(
            value.get("result").is_none(),
            "错误响应里不该出现 result 字段，实际: {value}"
        );
    }

    #[test]
    fn tools_call_without_params_is_invalid_params() {
        let server = Server::new();
        let value = call(&server, r#"{"jsonrpc":"2.0","id":6,"method":"tools/call"}"#);
        assert_eq!(value["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let server = Server::new();
        let value = call(&server, r#"{"jsonrpc":"2.0","id":7,"method":"resources/list"}"#);
        assert_eq!(value["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn broken_json_reports_parse_error_with_null_id() {
        let server = Server::new();
        let value = call(&server, "{not json");
        assert_eq!(value["error"]["code"], super::super::jsonrpc::PARSE_ERROR);
        assert!(value["id"].is_null());
    }

    #[test]
    fn empty_input_is_ignored() {
        let server = Server::new();
        assert!(server.handle_line("   ").is_none());
    }

    #[test]
    fn ping_replies_with_empty_object() {
        let server = Server::new();
        let value = call(&server, r#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#);
        assert_eq!(value["result"], json!({}));
    }

    /// 端到端：把"宿主会发的那几行"按顺序喂进去，回包必须都能往回解析。
    #[test]
    fn full_handshake_then_list() {
        let server = Server::new();
        let lines = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ];

        let mut replies = Vec::new();
        for line in lines {
            if let Some(out) = server.handle_line(line) {
                replies.push(out);
            }
        }

        assert_eq!(replies.len(), 2, "通知不应产生回包");
        assert!(replies.iter().all(|out| serde_json::from_str::<Value>(out).is_ok()));
    }
}
