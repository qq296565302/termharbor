//! 数据库查询：借 SSH 通道在服务器上跑 `mysql` 客户端。
//!
//! 为什么不自己实现 MySQL 协议：要解决的是"偶尔查一条数据、看一眼表"，
//! 不是完整的数据库客户端。复用服务器上已有的 mysql 客户端，省掉驱动依赖、
//! 账号体系、结果类型映射三块工作。
//!
//! 代价写在明处：结果走文本协议，`NULL` 和字符串 `"NULL"` 在 `--batch` 下
//! 无法区分（要精确区分得改用 `--xml` 并引入 XML 解析，不划算）。
//!
//! 密码怎么送进去：走标准输入，配 `--defaults-extra-file=/dev/stdin`。
//! 命令行里只有路径没有凭据，`ps` 看不到，密码也不落盘、不进错误信息。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::Handle;

use crate::error::{AppError, AppResult, ErrCode};
use crate::sftp::pack::shell_quote;
use crate::ssh::client::{establish, ProbeHandler, ProbeRequest, ProbeResult, Purpose};
use crate::ssh::exec::exec_with_stdin;
use crate::storage::db_accounts::DbAccount;

/// 查询超时。比诊断命令宽松：大表上 `count(*)` 确实可能跑一会儿。
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// 输出上限。文本协议比二进制协议费字节，1MB 大约够几万行短行。
const MAX_OUTPUT: usize = 1024 * 1024;

/// 退出码探针。mysql 的错误写在 stderr、跟结果混在一起，
/// 光看内容分不清"查询出错"还是"查到了一行叫 ERROR 的数据"。
const EXIT_MARK: &str = "__th_exit:";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 输出超过上限被截断，前面的行仍然有效
    pub truncated: bool,
}

/// 查询用的 SSH 连接池，按连接 id 索引。
///
/// 查询是低频操作，但仍然值得缓存：连点几条语句时，每条都重新握手会让
/// 「点一下等一下」的节奏很难受。断了靠 `is_closed()` 发现并重建。
#[derive(Default)]
pub struct DbLinks {
    links: Mutex<HashMap<String, Arc<Handle<ProbeHandler>>>>,
}

impl DbLinks {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn handle(
        &self,
        connection_id: &str,
        probe: &ProbeRequest,
    ) -> AppResult<Arc<Handle<ProbeHandler>>> {
        let existing = self.lock().get(connection_id).cloned();
        if let Some(handle) = existing {
            if !handle.is_closed() {
                return Ok(handle);
            }
            // 断了的连接留着没用，删掉再重建，免得每查一次都白等一次超时
            self.forget(connection_id);
        }

        let established = establish(probe, Purpose::Session).await;
        let handle = match established {
            Ok(established) => Arc::new(established.handle),
            Err(result) => return Err(probe_failure(result)),
        };

        self.lock()
            .insert(connection_id.to_string(), Arc::clone(&handle));
        Ok(handle)
    }

    pub fn forget(&self, connection_id: &str) {
        self.lock().remove(connection_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Handle<ProbeHandler>>>> {
        self.links
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 在给定连接上执行一条 SQL。
pub(crate) async fn query(
    conn: &Handle<ProbeHandler>,
    account: &DbAccount,
    password: &str,
    sql: &str,
) -> AppResult<QueryResult> {
    let command = build_command(sql);
    let option_file = build_option_file(account, password);

    let output = exec_with_stdin(
        conn,
        &command,
        option_file.as_bytes(),
        QUERY_TIMEOUT,
        MAX_OUTPUT,
    )
    .await?;

    let (body, code) = split_exit(&output);
    if code.unwrap_or(0) != 0 {
        return Err(AppError::with_detail(
            ErrCode::DbQueryFailed,
            explain(&body),
        ));
    }

    let mut result = parse_batch(&body);
    result.truncated = result.truncated || output.len() >= MAX_OUTPUT;
    Ok(result)
}

/// 拼要执行的命令。`;` 后面那句是退出码探针 —— 内部拼装，不经过 MCP 的复合命令闸门。
fn build_command(sql: &str) -> String {
    format!(
        "mysql --defaults-extra-file=/dev/stdin --batch --default-character-set=utf8mb4 -e {} ; printf '\\n{EXIT_MARK}%d\\n' $?",
        shell_quote(sql)
    )
}

/// 拼 `[client]` 段。
///
/// 值统一用双引号包住并转义 `\` `"` 换行制表符：密码里带引号时，
/// 裸写会把 option file 解析错位，报出来的却是"Access denied"，很难查。
fn build_option_file(account: &DbAccount, password: &str) -> String {
    let mut text = String::new();
    text.push_str("[client]\n");
    text.push_str("host=127.0.0.1\n");
    text.push_str(&format!("port={}\n", account.port));
    text.push_str(&format!("user=\"{}\"\n", escape_option(&account.username)));
    text.push_str(&format!("password=\"{}\"\n", escape_option(password)));
    if !account.database_name.is_empty() {
        text.push_str(&format!(
            "database=\"{}\"\n",
            escape_option(&account.database_name)
        ));
    }
    text
}

fn escape_option(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// 把末尾的退出码探针切下来。
fn split_exit(output: &str) -> (String, Option<i32>) {
    let Some(index) = output.rfind(EXIT_MARK) else {
        return (output.trim_end_matches(['\n', '\r']).to_string(), None);
    };
    let code = output[index + EXIT_MARK.len()..].trim().parse::<i32>().ok();
    let body = output[..index]
        .trim_end_matches(['\n', '\r'])
        .trim_end()
        .to_string();
    (body, code)
}

/// 从 mysql 的 stderr 里挑出能给人看的那一行。
fn explain(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("ERROR ") {
            return line.to_string();
        }
    }
    if body.trim().is_empty() {
        "mysql 执行失败，但没有输出错误信息".to_string()
    } else {
        body.trim().to_string()
    }
}

/// 解析 `--batch` 的制表符分隔输出。
fn parse_batch(body: &str) -> QueryResult {
    // 末尾的换行会 split 出一个空元素，被当成"一行全是空列"
    let body = body.trim_end_matches('\n');
    if body.trim().is_empty() {
        // DDL / DML 没有结果集
        return QueryResult::default();
    }

    let mut lines = body.split('\n');
    let header = lines.next().unwrap_or_default().trim_end_matches('\r');
    let columns: Vec<String> = header.split('\t').map(unescape).collect();

    let rows = lines
        .map(|line| {
            let line = line.trim_end_matches('\r');
            let mut cells: Vec<String> = line.split('\t').map(unescape).collect();
            // 列数不一致只会出现在被截断的末行：补齐或剪掉，保证表格能对齐
            if cells.len() < columns.len() {
                cells.resize(columns.len(), String::new());
            } else if cells.len() > columns.len() {
                cells.truncate(columns.len());
            }
            cells
        })
        .collect();

    QueryResult {
        columns,
        rows,
        truncated: false,
    }
}

/// `--batch` 下 mysql 只转义这四种字符，反着还原即可。
fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn probe_failure(result: ProbeResult) -> AppError {
    match result {
        ProbeResult::Failed(err) => err,
        ProbeResult::NeedsConfirmation { fingerprint, .. } => AppError::with_detail(
            ErrCode::SshHostKeyUnknown,
            format!("这台服务器还没确认过主机指纹（{fingerprint}），请先在图形界面里连接一次"),
        ),
        ProbeResult::HostKeyChanged { stored, .. } => AppError::with_detail(
            ErrCode::SshHostKeyMismatch,
            format!("主机密钥已变更，记录的是 {stored}，已阻断"),
        ),
        ProbeResult::CertificateUnsupported => {
            AppError::with_detail(ErrCode::SshAuthFailed, "服务器只提供证书认证，暂不支持")
        }
        ProbeResult::Connected(_) => AppError::internal("establish 返回了 Connected，不符合预期"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> DbAccount {
        DbAccount::new("c1", "root", b"blob".to_vec(), 13036).with_database("sgcc_ops")
    }

    #[test]
    fn option_file_carries_every_field() {
        let text = build_option_file(&account(), "p@ss");
        assert!(text.contains("host=127.0.0.1"));
        assert!(text.contains("port=13036"));
        assert!(text.contains("user=\"root\""));
        assert!(text.contains("password=\"p@ss\""));
        assert!(text.contains("database=\"sgcc_ops\""));
    }

    /// 密码里带引号和反斜杠时不能把 option file 截断 —— 否则报的是
    /// "Access denied"，排查方向会完全跑偏。
    #[test]
    fn option_file_escapes_quotes_and_backslashes() {
        let text = build_option_file(&account(), "a\"b\\c");
        assert!(text.contains(r#"password="a\"b\\c""#), "{text}");
    }

    #[test]
    fn empty_database_omits_the_key() {
        let rec = DbAccount::new("c1", "root", b"blob".to_vec(), 3306);
        assert!(!build_option_file(&rec, "x").contains("database="));
    }

    #[test]
    fn batch_output_splits_into_columns_and_rows() {
        let result = parse_batch("user\thost\nroot\tlocalhost\nops\t%\n");
        assert_eq!(result.columns, vec!["user", "host"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0], vec!["root", "localhost"]);
        assert_eq!(result.rows[1], vec!["ops", "%"]);
    }

    #[test]
    fn batch_output_restores_escaped_tabs_and_newlines() {
        let result = parse_batch("a\tb\nx\\ty\tline1\\nline2\n");
        assert_eq!(result.rows[0][0], "x\ty");
        assert_eq!(result.rows[0][1], "line1\nline2");
    }

    #[test]
    fn empty_output_means_no_result_set() {
        let result = parse_batch("\n");
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
    }

    #[test]
    fn exit_marker_is_cut_off_the_body() {
        let (body, code) = split_exit("col\nv\n\n__th_exit:0\n");
        assert_eq!(code, Some(0));
        assert_eq!(body, "col\nv");
    }

    #[test]
    fn non_zero_exit_is_carried_out() {
        let (_, code) = split_exit("ERROR 1045 (28000): Access denied\n__th_exit:1\n");
        assert_eq!(code, Some(1));
    }

    #[test]
    fn explain_picks_the_error_line() {
        let message = explain("mysql: [Warning] x\nERROR 1146 (42S02): Table 'a.b' doesn't exist");
        assert!(message.starts_with("ERROR 1146"), "{message}");
    }

    #[test]
    fn command_quotes_the_sql() {
        let command = build_command("select 'a b'");
        assert!(command.contains("--defaults-extra-file=/dev/stdin"));
        assert!(command.contains(EXIT_MARK));
        assert!(command.contains("'a b'"), "{command}");
    }
}
