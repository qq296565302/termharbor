//! JSON-RPC 2.0 的最小子集。
//!
//! 只覆盖 MCP 用得到的部分：请求、响应、错误。不实现批量请求（数组）——
//! MCP 的 stdio 传输里宿主不会发批量包，为一个用不到的分支写解析只会增加出错面。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 错误码。数值与 JSON-RPC 2.0 规范一致，客户端（宿主）按这些数字分类处理。
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

#[derive(Debug, Deserialize)]
pub struct Request {
    /// 规范要求 `"2.0"`。这里只做记录不校验：宿主写法五花八门，
    /// 为这个字段拒绝整条请求，收益远小于代价。
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// 通知没有 id。用 `Option` 区分"有 id 要回"和"通知不用回"。
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl Response {
    /// 成功响应。`id` 原样回传：宿主靠它把响应和请求对上。
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self::from_error(id, RpcError::new(code, message))
    }

    /// 错误响应：**不能再带 result 字段**（规范要求二者互斥）。
    /// 之前用 `..Response::ok(id, Value::Null)` 拼，序列化出来会多一个 `"result":null`，
    /// 严格的宿主会判为协议错误。
    pub fn from_error(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }

    /// 解析失败时连 id 都拿不到，规范要求回 `null`。
    pub fn parse_failed(message: impl Into<String>) -> Self {
        Self::err(Value::Null, PARSE_ERROR, message)
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|err| {
            // 正常构造的响应不会走到这里（全是 Value / &str）。真走到了，也宁可回一条
            // 合法的错误帧让宿主能继续，而不是让进程吐半行 JSON 出去。
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": INTERNAL_ERROR, "message": format!("响应序列化失败: {err}") }
            })
            .to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_without_id_is_a_notification() {
        let req: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .expect("通知也要能解析");
        assert!(req.id.is_none());
        assert!(req.params.is_none());
    }

    #[test]
    fn success_response_carries_result_only() {
        let line = Response::ok(Value::from(7), serde_json::json!({"ok": true})).to_line();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["ok"], true);
        assert!(value.get("error").is_none(), "成功响应里不该出现 error 字段");
    }

    #[test]
    fn error_response_carries_code() {
        let line = Response::err(Value::from(1), METHOD_NOT_FOUND, "没有这个方法").to_line();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(value["error"]["message"], "没有这个方法");
        assert!(value.get("result").is_none(), "错误响应里不该出现 result 字段");
    }

    #[test]
    fn parse_failure_uses_null_id() {
        let value: Value = serde_json::from_str(&Response::parse_failed("坏 JSON").to_line()).unwrap();
        assert!(value["id"].is_null(), "解析失败时规范要求 id 为 null");
        assert_eq!(value["error"]["code"], PARSE_ERROR);
    }
}
