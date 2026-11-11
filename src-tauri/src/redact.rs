//! 脱敏工具（约束 A-07 / SEC-06）。
//!
//! 两件事：
//! 1. [`redact`]：把一段自由文本里疑似凭据的片段打码，用于错误 detail 与日志；
//! 2. [`Redacted`]：把敏感值包起来，`Debug`/`Display` 永远输出 `***`，
//!    从根本上防止 `tracing::info!(?password)` 这类手滑。
//!
//! 这里是**启发式**，不追求完备。真正的防线是：敏感值从不以明文形式流经错误与日志路径。

use std::fmt;
use std::ops::Deref;

const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "access_key",
];

const MASK: &str = "***";

/// 把文本中形如 `password=xxx` / `Authorization: xxx` 的片段替换为 `password=***`。
///
/// 分隔符决定取值边界：
/// - `=`：值截到下一个空白，保留后续字段便于排查；
/// - `:`：值截到行尾或引号，因为 `Authorization: Bearer abc.def` 这类值本身含空格。
pub fn redact(input: &str) -> String {
    if input.contains("PRIVATE KEY-----") {
        return "[REDACTED_PEM]".to_string();
    }

    let chars: Vec<char> = input.chars().collect();
    let lowered: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    'scan: while i < chars.len() {
        for key in SENSITIVE_KEYS {
            let klen = key.chars().count();
            if i + klen > lowered.len() {
                continue;
            }
            if lowered[i..i + klen].iter().copied().ne(key.chars()) {
                continue;
            }

            let key_start = i;
            out.extend(&chars[i..i + klen]);
            i += klen;

            // 吃掉分隔符，记录是哪种
            let sep_start = i;
            let mut colon = false;
            let mut has_sep = false;
            while i < chars.len() && matches!(chars[i], ' ' | '=' | ':' | '"' | '\'') {
                match chars[i] {
                    ':' => {
                        colon = true;
                        has_sep = true;
                    }
                    '=' => has_sep = true,
                    _ => {}
                }
                i += 1;
            }
            out.extend(&chars[sep_start..i]);
            if !has_sep {
                // 只有空格/引号。`--password abc` 这种长选项确实用空格分隔值，
                // 但 `echo password abc` 就是普通句子 —— 只有前面带 `--` 才算。
                let dashed =
                    key_start >= 2 && chars[key_start - 1] == '-' && chars[key_start - 2] == '-';
                if !dashed {
                    // 这不是键值对（例如 "token missing"），原样继续
                    continue 'scan;
                }
                let value_start = i;
                while i < chars.len() && !is_value_end(chars[i], false) {
                    i += 1;
                }
                if i > value_start {
                    out.push_str(MASK);
                }
                continue 'scan;
            }

            let value_start = i;
            while i < chars.len() && !is_value_end(chars[i], colon) {
                i += 1;
            }
            if i > value_start {
                out.push_str(MASK);
            }
            continue 'scan;
        }
        out.push(chars[i]);
        i += 1;
    }

    out
}

fn is_value_end(c: char, colon: bool) -> bool {
    if colon {
        matches!(c, '\n' | '\r' | '"' | '\'' | ',' | ';' | ')' | ']' | '}')
    } else {
        c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
    }
}

/// 命令行专用脱敏，在 [`redact`] 之上补「短选项紧跟密码」。
///
/// 为什么不直接塞进 `redact`：`-p` 在命令行里太常见了 —— `mkdir -p`、`ssh -p 22`、
/// `ps -p 1234` 后面跟的都不是密码。所以只对**确定用短选项传密码**的几个程序做处理，
/// 靠程序名限定范围，不靠猜。
///
/// 只用于审计里的命令原文。返回值可能已经和原命令不同，**不能拿去执行**。
pub fn redact_command(command: &str) -> String {
    let base = redact(command);

    let mut tokens: Vec<String> = base.split_whitespace().map(str::to_string).collect();
    let program = tokens
        .first()
        .map(|token| token.rsplit('/').next().unwrap_or(token).to_string())
        .unwrap_or_default();

    let Some((flag, joined)) = short_password_flag(&program) else {
        return base;
    };

    let mut touched = false;
    for index in 0..tokens.len() {
        let token = tokens[index].clone();
        if joined {
            // `-pSECRET` 形式。单独的 `-p` 是「交互输入密码」，后面压根没有值。
            if token.len() > flag.len() && token.starts_with(flag) {
                tokens[index] = format!("{flag}{MASK}");
                touched = true;
            }
        } else if token == flag {
            // `-a SECRET` 形式，值在下一个 token
            if let Some(next) = tokens.get(index + 1) {
                if !next.starts_with('-') {
                    tokens[index + 1] = MASK.to_string();
                    touched = true;
                }
            }
        }
    }

    if touched {
        tokens.join(" ")
    } else {
        base
    }
}

/// 程序名 → (短选项, 是否紧跟值)。只收确定语义的几个。
fn short_password_flag(program: &str) -> Option<(&'static str, bool)> {
    match program {
        "mysql" | "mysqldump" | "mysqladmin" | "mysqlimport" | "mysqlshow" | "mariadb"
        | "mariadb-dump" => Some(("-p", true)),
        "redis-cli" => Some(("-a", false)),
        _ => None,
    }
}

/// 敏感值包装器：可以正常传递，但永远不会被打进日志。
#[derive(Clone, PartialEq, Eq)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Redacted<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_equals_form() {
        assert_eq!(
            redact("password=hunter2 host=1.2.3.4"),
            "password=*** host=1.2.3.4"
        );
        assert_eq!(redact("passphrase = abc123"), "passphrase = ***");
    }

    #[test]
    fn masks_header_form_until_end_of_line() {
        // 冒号形式的值本身可能含空格，所以要吃到行尾
        assert_eq!(
            redact("Authorization: Bearer abc.def"),
            "Authorization: ***"
        );
        assert_eq!(
            redact("Authorization: Bearer abc\nnext=1"),
            "Authorization: ***\nnext=1"
        );
    }

    #[test]
    fn masks_whole_pem_block() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(redact(pem), "[REDACTED_PEM]");
    }

    #[test]
    fn keeps_plain_and_chinese_text() {
        let s = "无法连接到服务器 10.0.0.1:22，耗时 3s";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn leaves_keyword_without_separator_alone() {
        assert_eq!(redact("token missing"), "token missing");
    }

    /// 长选项用空格分隔值也是密码。但只有 `--` 开头才算，否则会把英文句子打码。
    #[test]
    fn masks_double_dash_option_with_space() {
        assert_eq!(
            redact("deploy --password hunter2 --verbose"),
            "deploy --password *** --verbose"
        );
        assert_eq!(redact("--token abc123"), "--token ***");
        // 普通句子不能误伤
        assert_eq!(redact("echo password abc"), "echo password abc");
        // 长选项后面没值时不补 ***
        assert_eq!(redact("--password"), "--password");
    }

    /// `-p` 是 mysql 的密码，也是 `mkdir -p` 的「创建父目录」。
    /// 靠程序名限定范围，不能靠猜。
    #[test]
    fn redacts_mysql_short_password() {
        let out = redact_command("mysql -uroot -pS3cret db");
        assert!(!out.contains("S3cret"), "密码漏了: {out}");
        assert!(out.contains("-uroot"), "非敏感参数不该被吃掉: {out}");
        assert!(out.contains("db"), "库名要留着: {out}");
    }

    #[test]
    fn redacts_redis_cli_password() {
        let out = redact_command("redis-cli -h 10.0.0.1 -a hunter2 ping");
        assert!(!out.contains("hunter2"), "密码漏了: {out}");
        assert!(out.contains("10.0.0.1"));
        assert!(out.contains("ping"));
    }

    /// 最关键的一条：不能把普通命令打码，否则审计就没法看了。
    #[test]
    fn ordinary_commands_are_left_alone() {
        for cmd in [
            "mkdir -p /var/log/app",
            "ssh -p 2222 deploy@10.0.0.1",
            "ps -p 1234 -o pid,cmd",
            "tar -pxzf /tmp/a.tgz",
            "df -h",
            "systemctl status nginx",
        ] {
            assert_eq!(redact_command(cmd), cmd, "不该改动：{cmd}");
        }
    }

    /// 单独的 `-p` 是「交互输入密码」，后面没有值，别把下一个参数吃掉。
    #[test]
    fn bare_short_flag_does_not_eat_next_argument() {
        assert_eq!(redact_command("mysql -u root -p"), "mysql -u root -p");
    }

    /// 审计里的命令会再过一次 `redact`，结果必须稳定。
    #[test]
    fn redacting_twice_changes_nothing() {
        let once = redact_command("mysqldump -uroot -pabc123 --all-databases");
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn redacted_debug_never_leaks() {
        let secret = Redacted::new("hunter2".to_string());
        assert_eq!(format!("{secret:?}"), MASK);
        assert_eq!(format!("{secret}"), MASK);
        assert_eq!(secret.len(), 7); // Deref 仍可用
    }
}
