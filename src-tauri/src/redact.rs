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
                // 只有空格/引号，说明这不是键值对（例如 "token missing"），原样继续
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

    #[test]
    fn redacted_debug_never_leaks() {
        let secret = Redacted::new("hunter2".to_string());
        assert_eq!(format!("{secret:?}"), MASK);
        assert_eq!(format!("{secret}"), MASK);
        assert_eq!(secret.len(), 7); // Deref 仍可用
    }
}
