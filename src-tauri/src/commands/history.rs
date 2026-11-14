//! 命令历史 / 路径历史 command。
//!
//! 这一层只做参数校验与转发（对齐 `commands/mod.rs` 的纪律），真正的 SQL 在
//! `storage/history.rs`：
//! - kind 白名单、value 去空白/空值静默、value 与 connection_id 按字符数截断、
//!   limit 默认值，都在 `await` 之前算完，结果 `move` 进闭包；
//! - 所有 DB 访问一律走 `state.db.call(...)` —— 它内部在 `spawn_blocking` 里拿
//!   `Mutex` 锁，rusqlite 是同步库，禁止在 async 上下文里直连（约束 A-05）。
//!   全程不手动持有 `MutexGuard` 跨 `await`。
//! - storage 层返回 `rusqlite::Result`，借 `From<rusqlite::Error> for AppError`
//!   通过 `?` 转成 `AppResult`（`db.rs` 的单测里已有同样写法）。
//! - 命令内容可能是敏感信息，校验错误的 detail 用固定文案，绝不回显 value/kind 原文。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::history::{self, HistoryEntry};

/// 允许的 kind，与 `003_history.sql` 注释里的 `'command' | 'path'` 对齐。
const KINDS: [&str; 2] = ["command", "path"];
/// value 上限，按 `chars().count()` 计（字符数而非字节数），超了截断不报错。
const MAX_VALUE_CHARS: usize = 4096;
/// connection_id 上限，同样按字符数。
const MAX_CONNECTION_ID_CHARS: usize = 128;
/// complete 未指定 limit 时的默认值。
const DEFAULT_COMPLETE_LIMIT: u32 = 50;
/// complete 的 limit 范围；storage 层还会再兜底夹一次，防其它调用方传越界值。
const MIN_COMPLETE_LIMIT: u32 = 1;
const MAX_COMPLETE_LIMIT: u32 = 500;

/// kind 只接受白名单里的两个值，非法值返回校验类错误（`VALIDATION_ERROR`）。
fn validate_kind(kind: &str) -> AppResult<()> {
    if KINDS.contains(&kind) {
        Ok(())
    } else {
        // detail 固定文案，不带 kind 原文：命令历史可能含密码，不能随错误漂到日志/UI
        Err(AppError::validation("history kind 只接受 command 或 path"))
    }
}

/// 按字符数截断而非字节数，多字节字符（中文、emoji）不会被从中间劈开。
fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// 列出某个 kind 的全部历史（无条数上限，按时间倒序）。
#[tauri::command]
pub async fn history_list(
    state: State<'_, AppState>,
    kind: String,
) -> AppResult<Vec<HistoryEntry>> {
    validate_kind(&kind)?;
    state
        .db
        .call(move |conn| Ok(history::list(conn, &kind)?))
        .await
}

/// 写一条历史。空回车 / 纯空白静默成功不落库；超长 value 截断而非报错。
#[tauri::command]
pub async fn history_push(
    state: State<'_, AppState>,
    kind: String,
    value: String,
    connection_id: Option<String>,
) -> AppResult<()> {
    validate_kind(&kind)?;

    // 终端里空回车、纯空格不该写库：静默 Ok(()) 不报错、不打扰用户。
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    // 超长截断而不是报错，和设置页「越界静默 clamp」一致，避免打断输入。
    let value = truncate_chars(trimmed, MAX_VALUE_CHARS);

    // None 按 "" 存；去首尾空白并限制长度。先绑定到一个变量上再 trim，
    // 避免借用临时 String 生命周期不够清晰。
    let raw_connection_id = connection_id.unwrap_or_default();
    let connection_id = truncate_chars(raw_connection_id.trim(), MAX_CONNECTION_ID_CHARS);

    state
        .db
        .call(move |conn| Ok(history::push(conn, &kind, &value, &connection_id)?))
        .await
}

/// 清空某个 kind 的全部历史（storage 层的 `WHERE kind` 保证不误删另一类）。
#[tauri::command]
pub async fn history_clear(state: State<'_, AppState>, kind: String) -> AppResult<()> {
    validate_kind(&kind)?;
    state
        .db
        .call(move |conn| Ok(history::clear(conn, &kind)?))
        .await
}

/// 按 id 删除一条历史（storage 层带 kind 条件）。
#[tauri::command]
pub async fn history_remove(state: State<'_, AppState>, kind: String, id: String) -> AppResult<()> {
    validate_kind(&kind)?;
    state
        .db
        .call(move |conn| Ok(history::remove(conn, &kind, &id)?))
        .await
}

/// 取补全候选值（模糊匹配 + 去重，按「最常用/最近」排序）。
#[tauri::command]
pub async fn history_complete(
    state: State<'_, AppState>,
    kind: String,
    query: String,
    limit: Option<u32>,
) -> AppResult<Vec<String>> {
    validate_kind(&kind)?;
    let limit = limit
        .unwrap_or(DEFAULT_COMPLETE_LIMIT)
        .clamp(MIN_COMPLETE_LIMIT, MAX_COMPLETE_LIMIT);

    state
        .db
        .call(move |conn| Ok(history::complete(conn, &kind, &query, limit)?))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrCode;

    #[test]
    fn validate_kind_accepts_only_two_kinds() {
        assert!(validate_kind("command").is_ok());
        assert!(validate_kind("path").is_ok());
        assert!(validate_kind("").is_err());
        assert!(validate_kind("Command").is_err());
        assert!(validate_kind("path; DROP TABLE history").is_err());
    }

    #[test]
    fn invalid_kind_error_does_not_echo_input() {
        // 非法值里塞一段像命令/密码的内容，错误 detail 绝不能把它带出去
        let err = validate_kind("secret-password-cmd").unwrap_err();
        assert_eq!(err.code, ErrCode::ValidationError);
        let detail = err.detail.as_deref().unwrap_or("");
        assert!(
            !detail.contains("secret-password-cmd"),
            "detail 泄漏了输入: {detail}"
        );
    }

    #[test]
    fn truncate_chars_counts_chars_not_bytes() {
        // 每个中文 3 字节：若按字节截会把字符劈坏，按字符截则完整保留
        let s = "命令".repeat(3000); // 6000 个字符
        let out = truncate_chars(&s, MAX_VALUE_CHARS);
        assert_eq!(out.chars().count(), MAX_VALUE_CHARS);
        assert!(out.is_char_boundary(out.len()));

        // 短串原样返回
        assert_eq!(truncate_chars("ls -la", MAX_VALUE_CHARS), "ls -la");
    }
}
