//! MCP 审计日志。
//!
//! JSONL，**只追加**。成功和拒绝都要记 —— 被拒绝的才是最有价值的那部分：
//! 能看出模型有没有在试探边界、是谁在试。
//!
//! 两个纪律：
//! 1. 参数在写入前过 `redact()`，防止 `-p密码` 这类东西进日志；
//! 2. `clientInfo` 只做追溯，绝不参与任何放行判断（agent 自报的东西不可信）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lease::LeaseStamp;
use crate::redact::redact;
use crate::state::now_millis;

pub const FILE_NAME: &str = "audit.jsonl";
const MAX_ARGS_CHARS: usize = 2_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub ts_ms: u64,
    /// 宿主 initialize 时自报的名字，例如 `intranet-agent/1.2`
    pub client: String,
    pub tool: String,
    /// 连接名或 id；全局工具（list_connections）为 "-"
    pub connection: String,
    /// 已脱敏、已截断的参数摘要
    pub args: String,
    pub decision: String,
    /// 拒绝原因，成功时为 "-"
    pub reason: String,
    pub duration_ms: u64,
    pub bytes: usize,
    pub result: String,
    /// 这次调用是**哪一份租约**授权的（pid + 心跳时间）。
    ///
    /// 老记录里没有这个字段，`default` 兜住。`None` 有两种含义：没开在线校验、
    /// 或者读的时候压根没有租约文件 —— 后者会以 `no_live_session` 拒绝，
    /// 所以看拒绝行时不用纠结这一栏是空的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<LeaseStamp>,
}

impl AuditEntry {
    pub fn allowed(tool: &str, connection: &str, args: &str, client: &str) -> Self {
        Self {
            ts_ms: now_millis(),
            client: client.into(),
            tool: tool.into(),
            connection: connection.into(),
            args: summarize(args),
            decision: "allowed".into(),
            reason: "-".into(),
            duration_ms: 0,
            bytes: 0,
            result: "ok".into(),
            lease: None,
        }
    }

    pub fn denied(tool: &str, connection: &str, args: &str, client: &str, reason: &str) -> Self {
        Self {
            ts_ms: now_millis(),
            client: client.into(),
            tool: tool.into(),
            connection: connection.into(),
            args: summarize(args),
            decision: "denied".into(),
            reason: reason.into(),
            duration_ms: 0,
            bytes: 0,
            result: "denied".into(),
            lease: None,
        }
    }

    /// 挂上这次调用的租约指纹。链式调用，构造点不必关心字段顺序。
    pub fn with_lease(mut self, lease: Option<LeaseStamp>) -> Self {
        self.lease = lease;
        self
    }

    pub fn finish(mut self, started_ms: u64, bytes: usize, result: impl Into<String>) -> Self {
        self.duration_ms = now_millis().saturating_sub(started_ms);
        self.bytes = bytes;
        self.result = result.into();
        self
    }
}

/// 脱敏 + 截断。日志不是给人看小说用的，够定位就行。
fn summarize(args: &str) -> String {
    let redacted = redact(args);
    let mut out: String = redacted.chars().take(MAX_ARGS_CHARS).collect();
    if redacted.chars().count() > MAX_ARGS_CHARS {
        out.push_str("…(truncated)");
    }
    out
}

pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(product_dir: &Path) -> Self {
        Self {
            path: product_dir.join(FILE_NAME),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一行。写失败只 warn —— 审计不该让查询本身失败，但也别悄悄丢，
    /// 所以在 stderr 留一句。
    pub fn record(&self, entry: &AuditEntry) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // 简单轮转：文件太大就改名留档，避免无限增长
            if let Ok(meta) = std::fs::metadata(&self.path) {
                if meta.len() > MAX_FILE_BYTES {
                    let old = self.path.with_extension("jsonl.1");
                    let _ = std::fs::rename(&self.path, old);
                }
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            let line = serde_json::to_string(entry).unwrap_or_else(|_| "{}".into());
            writeln!(file, "{line}")?;
            Ok(())
        })();

        if let Err(err) = result {
            eprintln!("[termharbor-mcp] 审计日志写入失败: {err}");
            tracing::warn!(error = %err, "审计日志写入失败");
        }
    }

    /// 最近 N 条调用（放行与拒绝都算），时间正序（最新在末尾）。
    ///
    /// 设置页的「最近调用」用它。为什么必须包含放行：租约指纹的价值在于核对
    /// 「那次是谁放行的」，只列拒绝的话，用伪造租约蒙混过去的那次恰恰不会出现。
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<AuditEntry> = raw
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<AuditEntry>(line).ok())
            .take(limit)
            .collect();
        out.reverse();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("th-mcp-audit-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_are_appended_as_jsonl() {
        let dir = temp_dir("append");
        let log = AuditLog::new(&dir);
        log.record(&AuditEntry::allowed("exec", "web-01", "df -h", "agent/1"));
        log.record(&AuditEntry::denied(
            "exec",
            "web-01",
            "rm -rf /",
            "agent/1",
            "command_not_allowed",
        ));

        let raw = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"decision\":\"allowed\""));
        assert!(lines[1].contains("command_not_allowed"));
    }

    #[test]
    fn tail_keeps_allowed_and_denied_in_order() {
        let dir = temp_dir("tail");
        let log = AuditLog::new(&dir);
        log.record(&AuditEntry::denied("exec", "a", "bad1", "c", "x"));
        log.record(&AuditEntry::allowed("exec", "a", "df -h", "c"));
        log.record(&AuditEntry::denied("exec", "a", "bad2", "c", "y"));

        let tail = log.tail(10);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].args, "bad1");
        assert_eq!(tail[1].args, "df -h");
        assert_eq!(tail[2].args, "bad2");
    }

    /// 0.2.1 及更早写的日志没有 lease 字段，升级后必须还能读出来。
    #[test]
    fn old_entries_without_lease_still_parse() {
        let dir = temp_dir("old");
        let log = AuditLog::new(&dir);
        std::fs::write(
            log.path(),
            r#"{"tsMs":1,"client":"c","tool":"exec","connection":"a","args":"df","decision":"allowed","reason":"-","durationMs":1,"bytes":2,"result":"ok"}
"#,
        )
        .unwrap();

        let tail = log.tail(5);
        assert_eq!(tail.len(), 1);
        assert!(tail[0].lease.is_none(), "老记录没有指纹，不能解析失败");
    }

    #[test]
    fn lease_stamp_round_trips_through_jsonl() {
        let dir = temp_dir("lease");
        let log = AuditLog::new(&dir);
        log.record(
            &AuditEntry::allowed("exec", "a", "df", "c").with_lease(Some(LeaseStamp {
                pid: 42,
                beat_ms: 1_000,
                age_ms: 12,
            })),
        );

        let raw = std::fs::read_to_string(log.path()).unwrap();
        assert!(raw.contains("\"pid\":42"), "指纹要落盘：{raw}");
        assert!(raw.contains("\"ageMs\":12"), "心跳年龄要落盘：{raw}");
        assert_eq!(log.tail(1)[0].lease.as_ref().unwrap().pid, 42);
    }

    #[test]
    fn args_are_redacted_and_truncated() {
        let long = "x".repeat(MAX_ARGS_CHARS + 100);
        let out = summarize(&long);
        assert!(out.chars().count() <= MAX_ARGS_CHARS + 20);
        assert!(out.contains("truncated"));
    }
}
