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
const INSTRUCTIONS: &str = "TermHarbor 终端港：通过本机已保存的连接操作服务器。\
**可用范围由用户对每台机器单独设定**：没勾的连接看不到；\
勾了的连接默认可用全部工具，用户也可以再收窄到某些工具/目录/命令。\
先调用 list_connections 看有哪些连接可用，然后按需使用：\
sftp_list 列目录、sftp_read 读文本、sftp_write 写文本、\
sftp_upload 与 sftp_download 和本机互传文件、sftp_mkdir 建目录、sftp_rename 改名移动、\
load_snapshot 看负载、exec 执行命令。\
不会也不能返回密码、私钥或主机指纹。\
\
纪律（很重要）：\
1. 只用本服务的工具操作。不要启动图形界面、不要自己扫描端口、不要让用户去界面里手动点按钮。\
2. list_connections 只是「已保存的连接」，不表示在线，也不表示能登录成功；不要据此断言在线状态。\
3. exec 只接受单条命令。需要多步就分多次调用，不要试图用 ; 管道 重定向 $() 串起来 —— 这类命令会被拒绝。\
   装包、部署这种多步操作，更好的做法是让用户放一个脚本，然后只调用那个脚本。\
4. 被拒绝时把原因原样转告用户，让用户去「设置 → MCP 接入」调整；\
   不要改用别的工具、别的路径或拼命令绕过。\
5. 工具报错时，把错误原文和它给的处理建议原样转告用户并停下，不要自行改用别的方式重试。\
6. 写文件、上传、重命名会改变服务器状态。动手前确认目标路径；\
   要覆盖已有文件时先 sftp_read 看看原来是什么，不要盲写。";

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

        // 宿主自报的身份，只写进审计日志做追溯。
        // **不参与任何放行判断** —— agent 想报什么就能报什么，还容易被注入。
        if let Some(client) = params.and_then(|value| value.get("clientInfo")) {
            let name = client.get("name").and_then(Value::as_str).unwrap_or("?");
            let version = client.get("version").and_then(Value::as_str).unwrap_or("?");
            self.ctx.set_client(format!("{name}/{version}"));
        }

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
                .contains("用户对每台机器单独设定"),
            "开场白里要说清权限来自本机对每台机器的设定"
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
    async fn tools_list_covers_every_capability() {
        let value = call(
            &server(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        )
        .await;
        let tools = value["result"]["tools"]
            .as_array()
            .expect("要有 tools 数组");
        assert_eq!(tools.len(), crate::mcp::config::ALL_TOOLS.len());
        assert_eq!(tools[0]["name"], "list_connections");
        assert!(tools[0]["inputSchema"]["type"] == "object");

        // 读写都要有，缺一个 agent 就只能看不能干
        for name in ["sftp_read", "sftp_write", "sftp_upload", "sftp_download"] {
            assert!(tools.iter().any(|tool| tool["name"] == name), "少了 {name}");
        }
    }

    /// 造一台"数据库里有、但白名单没开"的机器。
    async fn seeded_server() -> (Server, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("th-mcp-seeded-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let ctx = Context::for_tests(dir.clone());
        let record = crate::storage::connections::ConnectionRecord {
            id: "conn-1".into(),
            name: "内网测试机".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            auth_type: crate::storage::connections::AuthType::Password,
            secret_blob: None,
            key_path: None,
            key_pass_blob: None,
            legacy_algorithms: false,
            last_used_at: None,
            created_at: 0,
            updated_at: 0,
        };
        ctx.db()
            .unwrap()
            .call(move |conn| crate::storage::connections::insert(conn, &record))
            .await
            .unwrap();
        (Server::new(ctx), dir)
    }

    /// 默认全关：数据库里有机器，agent 一台都看不到，也调不动任何工具。
    #[tokio::test]
    async fn fail_closed_hides_connections_and_denies_tools() {
        let (server, dir) = seeded_server().await;

        let list = call(
            &server,
            r#"{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}"#,
        )
        .await;
        assert_eq!(
            list["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .trim(),
            "[]"
        );

        let denied = call(
            &server,
            r#"{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"sftp_list","arguments":{"connection":"内网测试机","path":"/var/log"}}}"#,
        )
        .await;
        // 未开放的连接要表现得像"不存在"，避免 agent 枚举库里的机器
        assert_eq!(denied["error"]["code"], crate::mcp::jsonrpc::INVALID_PARAMS);

        // 但审计里如实记录了真实原因

        let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(
            audit.contains("connection_disabled"),
            "审计没记拒绝原因：{audit}"
        );
    }

    /// 打开白名单后连接可见；但没配置过的路径仍然被拦在闸门层（不会真去连 SSH）。
    #[tokio::test]
    async fn enabling_a_connection_makes_it_visible_but_paths_stay_gated() {
        let (server, _dir) = seeded_server().await;
        let mut cfg = crate::mcp::config::McpConfig::default();
        cfg.connections.insert(
            "内网测试机".into(),
            crate::mcp::config::ConnectionPolicy {
                enabled: true,
                tools: vec!["sftp_list".into()],
                paths: vec!["/var/log".into()],
                ..Default::default()
            },
        );
        server.ctx.config().save(&cfg).unwrap();

        let list = call(
            &server,
            r#"{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}"#,
        )
        .await;
        assert!(list["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("内网测试机"));

        let denied = call(
            &server,
            r#"{"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"sftp_list","arguments":{"connection":"内网测试机","path":"/etc"}}}"#,
        )
        .await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("path_not_allowed"));
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
