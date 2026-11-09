//! 执行档的命令闸门。
//!
//! 这里只做**纯判定**：不碰网络、不碰数据库，所以每一条规则都能单测。
//! 判定顺序刻意固定：先看复合字符，再匹配白名单 —— 先匹配的话
//! `df -h; rm -rf /` 会先命中 `df -h` 前缀，后面的东西就漏过去了。

use serde::Serialize;

/// 拒绝原因。会原样写进审计日志，也会给 agent 看，所以用短英语常量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// 连接没在 mcp.json 里启用
    ConnectionDisabled,
    /// 这台机器没开这个工具
    ToolNotAllowed,
    /// sftp_list 的路径不在允许前缀里
    PathNotAllowed,
    EmptyCommand,
    CompoundCommand,
    CommandNotAllowed,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::ConnectionDisabled => "connection_disabled",
            DenyReason::ToolNotAllowed => "tool_not_allowed",
            DenyReason::PathNotAllowed => "path_not_allowed",
            DenyReason::EmptyCommand => "empty_command",
            DenyReason::CompoundCommand => "compound_command",
            DenyReason::CommandNotAllowed => "command_not_allowed",
        }
    }
}

/// 一条命令能不能执行。
///
/// `allow` 留空 = 任意**单条**命令（默认档，0.1.28 起）。填了就是收窄，匹配的是
/// **分词后的 token 序列前缀**，不是字符串前缀：白名单 `df -h` 不该让 `df -hxxx`
/// 通过，但可以让 `df -h /var` 通过（多出来的参数属于同一条命令）。
///
/// 复合字符检查在任何情况下都执行 —— 它不是权限门槛，而是「一条命令」这个契约本身。
/// agent 要多步操作就多调几次，能力没有损失。
pub fn validate_command(command: &str, allow: &[String]) -> Result<(), DenyReason> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(DenyReason::EmptyCommand);
    }

    if has_shell_metachar(trimmed) {
        return Err(DenyReason::CompoundCommand);
    }

    if allow.is_empty() {
        return Ok(());
    }

    let asked: Vec<&str> = trimmed.split_whitespace().collect();
    let allowed = allow.iter().any(|pattern| {
        let expected: Vec<&str> = pattern.split_whitespace().collect();
        !expected.is_empty()
            && asked.len() >= expected.len()
            && expected
                .iter()
                .zip(asked.iter())
                .all(|(want, got)| want == got)
    });

    if allowed {
        Ok(())
    } else {
        Err(DenyReason::CommandNotAllowed)
    }
}

/// shell 元字符一律拒。
///
/// 这里宁可误伤：白名单本来就是给人显式配的只读命令，用不到重定向、管道、
/// 命令替换、后台执行。漏一个就等于给了半个 shell。
fn has_shell_metachar(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            ';' | '|'
                | '&'
                | '`'
                | '>'
                | '<'
                | '$'
                | '('
                | ')'
                | '{'
                | '}'
                | '\\'
                | '\n'
                | '\r'
                | '\0'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match_passes() {
        assert_eq!(validate_command("df -h", &allow(&["df -h"])), Ok(()));
    }

    #[test]
    fn extra_arguments_pass_but_only_as_full_tokens() {
        assert_eq!(validate_command("df -h /var", &allow(&["df -h"])), Ok(()));
        assert_eq!(
            validate_command("df -hxxx", &allow(&["df -h"])),
            Err(DenyReason::CommandNotAllowed)
        );
    }

    #[test]
    fn compound_commands_are_rejected_before_allowlist_matching() {
        let allow = allow(&["df -h"]);
        for cmd in [
            "df -h; rm -rf /",
            "df -h && cat /etc/shadow",
            "df -h | grep x",
            "df -h > /tmp/out",
            "df -h $(whoami)",
            "df -h `id`",
            "df -h\nrm -rf /",
            "df -h &",
        ] {
            assert_eq!(
                validate_command(cmd, &allow),
                Err(DenyReason::CompoundCommand),
                "该拦住：{cmd}"
            );
        }
    }

    #[test]
    fn empty_or_unlisted_is_rejected() {
        let allow = allow(&["df -h"]);
        assert_eq!(validate_command("", &allow), Err(DenyReason::EmptyCommand));
        assert_eq!(
            validate_command("   ", &allow),
            Err(DenyReason::EmptyCommand)
        );
        assert_eq!(
            validate_command("rm -rf /", &allow),
            Err(DenyReason::CommandNotAllowed)
        );
    }

    /// 白名单没配 = 任意单条命令都放行。这是 0.1.28 的默认档：
    /// 用户勾了连接就该能干活，不该先猜该填什么命令。
    #[test]
    fn empty_allowlist_permits_any_single_command() {
        for cmd in [
            "df -h",
            "apt-get install -y nginx",
            "systemctl restart nginx",
            "tar -xzf /tmp/a.tar.gz -C /opt",
            "python3 /opt/deploy.py release",
        ] {
            assert_eq!(validate_command(cmd, &[]), Ok(()), "应当放行: {cmd}");
        }
    }

    /// 但「单条」这个契约不放松：复合命令依然一律拒。
    #[test]
    fn empty_allowlist_still_refuses_compound_commands() {
        for cmd in [
            "df -h; rm -rf /",
            "apt-get update && apt-get install -y nginx",
            "cat x | grep y",
            "echo a > /etc/passwd",
            "rm -rf $(cat list.txt)",
        ] {
            assert_eq!(
                validate_command(cmd, &[]),
                Err(DenyReason::CompoundCommand),
                "该拦住：{cmd}"
            );
        }
    }

    #[test]
    fn multiple_patterns_are_checked_independently() {
        let allow = allow(&["df -h", "systemctl status nginx"]);
        assert_eq!(validate_command("systemctl status nginx", &allow), Ok(()));
        assert_eq!(
            validate_command("systemctl restart nginx", &allow),
            Err(DenyReason::CommandNotAllowed)
        );
    }
}
