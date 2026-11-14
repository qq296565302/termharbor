//! MCP stdio 启动凭证校验。
//!
//! 由来：`serve_stdio()` 过去构造完 `Context` 就直接读 stdin 上的 JSON-RPC，
//! 任何进程都能手工 spawn 这个无头二进制、发一条 `tools/call` 去碰用户已授权的
//! SSH 连接。stdio 天生没有「对端是谁」的概念，于是在启动阶段补一道凭证：
//! 宿主在设置页生成随机串写进 `mcp.json` 的 `hostToken`，spawn 时用
//! `--mcp-token <token>`（或 `TERMHARBOR_MCP_TOKEN` 环境变量）带进来，
//! 启动时与配置比对，不匹配就拒绝服务。
//!
//! 本模块只做「取 token + 字符串比对」这一件纯逻辑的事：不读文件、不读真实环境、
//! 不碰进程，配置值与环境值都由调用方传入。这样它跨平台零风险（无需任何
//! `#[cfg(...)]` 分支）、可完整单测；而「读 `hostToken` / 写 audit.jsonl /
//! 决定放不放行」统统留给 `serve_stdio` 的调用方，配合 [`LaunchStatus`] 三态处理。

use uuid::Uuid;

/// 启动凭证的校验结果。
///
/// 刻意做成三态而不是 `bool`：`NotConfigured` 是专门给老配置留的兼容档，
/// 让调用方可以「放行 + 写一条警告审计」，而不是把它硬塞成通过或拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchStatus {
    /// 凭证匹配，允许服务。
    Verified,
    /// 配置里没有 `hostToken`（空串 / 全空白）：老配置，允许但应写一条警告审计。
    ///
    /// 为什么不能直接判 `Rejected`：老版本配置里根本没有这个字段，一律拒绝等于
    /// 用一次版本升级静默收紧、把本来能正常用的 MCP 变成连不上，用户会以为坏了。
    /// 安全等级的收紧必须是显式动作（去设置页生成凭证），所以这里只报状态、
    /// 把「放行并留痕」的决定权交给调用方。
    NotConfigured,
    /// 传了 token 但与配置不匹配，或配置了 token 却没传：拒绝服务。
    Rejected,
}

/// 从进程命令行参数或环境变量里取 token 并与配置比对。
///
/// - 命令行：找 `--mcp-token` 后紧跟的那个参数；也接受 `--mcp-token=<value>`。
/// - 环境变量值由 `env_token` 传入（模块不自己读 `std::env`，保持纯函数、便于单测）。
/// - 两处都有时以命令行为准。
///
/// 状态判定：
/// - `expected` 为空串 / 全空白 → [`LaunchStatus::NotConfigured`]（不看 provided）。
/// - `expected` 非空、provided 缺失或不匹配 → [`LaunchStatus::Rejected`]。
/// - `expected` 非空且匹配 → [`LaunchStatus::Verified`]。
pub fn verify(expected: &str, args: &[String], env_token: Option<String>) -> LaunchStatus {
    // 老配置兼容优先，且直接短路：未配置状态下 provided 是什么都不影响结论。
    // 若在这里先去解析/比对 token，反而可能因为宿主随手带了个参数，把一个
    // 本该「放行 + 警告」的老配置误判成拒绝 —— 那正是要避免的静默改行为。
    if expected.trim().is_empty() {
        return LaunchStatus::NotConfigured;
    }

    // 命令行优先于环境变量：命令行是宿主本次 spawn 显式交给「这一个进程」的，
    // 作用域最窄、意图最明确；环境变量是继承来的，可能被父进程或别工具污染、
    // 也可能残留旧值。以显式参数为准，才不会出现「命令行换了新 token 却因环境里
    // 有个旧值一直不生效」这种极难排查的问题。命不到命令行才回退到环境变量。
    let provided = match token_from_args(args) {
        Some(from_cli) => Some(from_cli),
        None => env_token,
    };

    // 配了 token 却两处都没传 → 拒绝，绝不退化成放行，否则凭证形同虚设。
    let provided = match provided {
        Some(value) => value,
        None => return LaunchStatus::Rejected,
    };

    // 常量时间比较，见 [`constant_time_eq`]。
    if constant_time_eq(expected.as_bytes(), provided.as_bytes()) {
        LaunchStatus::Verified
    } else {
        LaunchStatus::Rejected
    }
}

/// 从命令行参数里取出 `--mcp-token` 的值，支持 `--mcp-token <value>` 与
/// `--mcp-token=<value>` 两种形式。取不到返回 `None`，由 [`verify`] 决定是否回退。
fn token_from_args(args: &[String]) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        let arg = arg.as_str();
        // 等号形式：等号之后的整个串都是值，包括空串（「传了但为空」，后续比对会拒绝）。
        if let Some(value) = arg.strip_prefix("--mcp-token=") {
            return Some(value.to_string());
        }
        // 空格形式：取紧跟的下一个参数。用 get + cloned 而不是索引，
        // 宿主漏写值时返回 None（回退环境变量），不会在库代码里 panic。
        if arg == "--mcp-token" {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// 常量时间字节比较：无论是否相等都遍历完整个切片，把差异累积到最后统一判断。
///
/// 为什么不能用 `==`：字符串比较会在**第一个不同字节**处提前返回，攻击者反复
/// 构造候选 token、测这次启动的响应耗时，就能逐字节猜出正确值（计时侧信道）。
/// 把差异 OR 进一个整数、循环里不做任何提前返回，耗时就只跟长度有关、跟内容无关。
/// 长度差异也并入结果：长度不算秘密，但长度不等必须判不匹配，且按较长者遍历，
/// 免得「长度不同时秒返回」又漏出信息。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        // get() 越界得 None，缺失的一侧按 0 参与运算；库代码里不留任何潜在 panic 点。
        let x = match a.get(i) {
            Some(v) => *v,
            None => 0,
        };
        let y = match b.get(i) {
            Some(v) => *v,
            None => 0,
        };
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

/// 生成一个新的随机凭证（供设置页「重新生成」用）。
///
/// 用两段 [`Uuid::now_v7`] 各取一半拼成 32 位十六进制字符串（simple 格式天然
/// 去掉连字符）。为什么取「后 16 位」而不是「前 16 位」：v7 UUID 的开头是毫秒时间戳，
/// 两次 `now_v7()` 几乎总落在同一毫秒，取前半段会让两个 token 前缀几乎一样、熵塌掉；
/// 后半段才是随机位。用 `chars().skip(16)` 而不是切片索引，避免任何越界 panic。
pub fn generate_token() -> String {
    let first = Uuid::now_v7().simple().to_string();
    let second = Uuid::now_v7().simple().to_string();
    first
        .chars()
        .skip(16)
        .chain(second.chars().skip(16))
        .collect()
}

/// 把启动结果写成一条审计用的中文说明（给 `audit.jsonl` 的 `result` 字段）。
///
/// 返回 `&'static str`：这三句话是编译期常量，调用方零分配、也不必再 `to_string`。
/// 用完整中文句子而不是 `verified` 这类代号，是为了人翻审计日志时能一眼看懂。
pub fn audit_note(status: LaunchStatus) -> &'static str {
    match status {
        LaunchStatus::Verified => "凭证校验通过",
        LaunchStatus::NotConfigured => "未配置启动凭证，请前往设置页生成凭证",
        LaunchStatus::Rejected => "启动凭证不匹配，已拒绝服务",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把 `&str` 列表转成 `verify` 需要的 `Vec<String>`。
    fn cli(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn empty_expected_is_not_configured_even_with_a_token() {
        // 未配置状态完全不看 provided：宿主随手带个参数也不该把老配置挡死。
        assert_eq!(
            verify(
                "",
                &cli(&["--mcp-token", "whatever"]),
                Some("whatever".to_string()),
            ),
            LaunchStatus::NotConfigured
        );
    }

    #[test]
    fn whitespace_only_expected_is_not_configured() {
        // 含全角空格 U+3000，验证用的是 trim 而不是只判 == ""。
        for blank in [" ", "   ", "\t", " \n ", "\u{3000}"] {
            assert_eq!(
                verify(blank, &[], None),
                LaunchStatus::NotConfigured,
                "全空白 expected 应视为未配置：{blank:?}"
            );
        }
    }

    #[test]
    fn matching_cli_token_is_verified() {
        // 夹杂其它参数时也能找到 --mcp-token 后紧跟的那个值。
        assert_eq!(
            verify(
                "s3cr3t",
                &cli(&["--verbose", "--mcp-token", "s3cr3t"]),
                None
            ),
            LaunchStatus::Verified
        );
    }

    #[test]
    fn mismatched_cli_token_is_rejected() {
        assert_eq!(
            verify("s3cr3t", &cli(&["--mcp-token", "wrong"]), None),
            LaunchStatus::Rejected
        );
    }

    #[test]
    fn configured_but_no_token_anywhere_is_rejected() {
        // 配了 hostToken 却什么都没传：不能退化成放行。
        assert_eq!(
            verify("s3cr3t", &cli(&["--verbose"]), None),
            LaunchStatus::Rejected
        );
        assert_eq!(verify("s3cr3t", &[], None), LaunchStatus::Rejected);
    }

    #[test]
    fn equals_form_is_verified() {
        assert_eq!(
            verify("s3cr3t", &cli(&["--mcp-token=s3cr3t"]), None),
            LaunchStatus::Verified
        );
    }

    #[test]
    fn env_token_is_the_fallback() {
        // 命令行没有 --mcp-token，回退到环境变量且匹配。
        assert_eq!(
            verify("s3cr3t", &[], Some("s3cr3t".to_string())),
            LaunchStatus::Verified
        );
    }

    #[test]
    fn mismatched_env_token_is_rejected() {
        assert_eq!(
            verify("s3cr3t", &[], Some("stale".to_string())),
            LaunchStatus::Rejected
        );
    }

    #[test]
    fn cli_wins_over_env_when_both_present() {
        // 命令行优先：命令行正确、环境里是旧值，应当通过。
        assert_eq!(
            verify(
                "s3cr3t",
                &cli(&["--mcp-token", "s3cr3t"]),
                Some("stale".to_string()),
            ),
            LaunchStatus::Verified
        );
    }

    #[test]
    fn cli_is_not_rescued_by_a_correct_env() {
        // 反向：命令行带了（错的）值就得以它为准，不能因环境里恰好有正确值
        // 就悄悄回退放行 —— 否则「命令行优先」名存实亡。
        assert_eq!(
            verify(
                "s3cr3t",
                &cli(&["--mcp-token", "wrong"]),
                Some("s3cr3t".to_string()),
            ),
            LaunchStatus::Rejected
        );
    }

    #[test]
    fn constant_time_eq_checks_length_and_content() {
        // 正确性边界：相等、同长不同字节、不同长度、含空串。
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn generate_token_is_32_hex_chars_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b, "连续两次生成的凭证必须不同");
        for token in [&a, &b] {
            assert_eq!(token.len(), 32, "凭证应为 32 位：{token}");
            assert!(
                token.bytes().all(|c| c.is_ascii_hexdigit()),
                "字符集必须全是十六进制字符：{token}"
            );
        }
    }

    #[test]
    fn audit_note_covers_all_three_statuses() {
        assert_eq!(audit_note(LaunchStatus::Verified), "凭证校验通过");
        assert!(
            audit_note(LaunchStatus::NotConfigured).contains("设置页"),
            "未配置文案要提醒去设置页生成凭证"
        );
        assert_eq!(
            audit_note(LaunchStatus::Rejected),
            "启动凭证不匹配，已拒绝服务"
        );
    }
}
