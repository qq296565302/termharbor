//! 工具清单与分发。
//!
//! M8-1 只做**只读**三件事：看连接列表、列远端目录、看负载快照。
//! `read_file` 虽然也是只读，但"能读服务器上任意文件"等于把内容外泄面放开了，
//! 单独立项再决定（开发计划 §17.2）。
//!
//! 这一版每个工具的实现都是"尚未实现"——第 1 步只验证协议链路是否通。
//! 真正的实现（要连 SSH、要开库）放在第 2 步，那时才会碰 `storage` / `ssh` 的重代码。

use serde::Serialize;
use serde_json::{json, Value};

use super::jsonrpc::{INVALID_PARAMS, RpcError};

/// 工具元信息。
///
/// `rename_all = "camelCase"` 不是风格问题：MCP 规范里字段就叫 `inputSchema`，
/// 少了这个属性会序列化成 `input_schema`，宿主找不到 schema 就把工具当无参数处理。
/// （第一版就是靠下面那条测试抓到的。）
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
            description: "列出 TermHarbor 里保存的服务器连接。只返回名称、主机、端口、用户名，\
                          不含任何密码、私钥或主机指纹。",
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

/// 调用一个工具。
///
/// 成功返回 MCP 约定的 `content` 数组；失败返回 JSON-RPC 错误（未知工具/参数不对）。
/// 注意区分：**工具自己执行失败**应该返回 `isError: true` 的 content，
/// 而不是协议错误 —— 那是给模型看的内容，不是通信故障。
pub fn call(name: &str, _arguments: Option<&Value>) -> Result<Value, RpcError> {
    let known = list().iter().any(|tool| tool.name == name);
    if !known {
        let available: Vec<&str> = list().iter().map(|tool| tool.name).collect();
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("未知工具 {name}；可用工具：{}", available.join(", ")),
        ));
    }

    // 第 1 步：协议通了就算成功，实现留给第 2 步
    Ok(json!({
        "content": [{
            "type": "text",
            "text": format!("{name} 尚未实现（M8-1 第 2 步）。当前版本只验证 MCP 协议链路。")
        }],
        "isError": true
    }))
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

        // list_connections 不收参数：多传就是参数错误，别静默忽略
        let list_conn = tools.iter().find(|t| t.name == "list_connections").unwrap();
        assert_eq!(list_conn.input_schema["additionalProperties"], false);
    }

    #[test]
    fn unknown_tool_is_an_invalid_params_error() {
        let err = call("rm_rf", None).expect_err("未知工具必须报错");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("sftp_list"), "错误信息里要列出可用工具");
    }

    #[test]
    fn known_tool_reports_not_implemented_as_content_error() {
        let value = call("list_connections", None).expect("已知工具不该走协议错误");
        assert_eq!(value["isError"], true);
        assert_eq!(value["content"][0]["type"], "text");
    }
}
