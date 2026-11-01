//! 检查 GitHub Releases 上有没有新版本。
//!
//! **这里不做真正的自动更新**，只做到「告诉你有没有新版 + 打开下载页」。
//! 原因见开发计划 R-2 / R-3：更新包没有代码签名时，SmartScreen 会拦静默安装，
//! "全自动更新"在没有证书的情况下实际会变成"自动失败"。等签名落地再谈安装。
//!
//! 网络请求放在 Rust 这边而不是前端 `fetch`：WebView 的 CSP 是 `default-src 'self'`，
//! 为了一个版本号去放开 `connect-src` 不划算；顺便还能把超时和响应大小都捏在手里。

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{AppError, AppResult, ErrCode};

/// 仓库地址是编译期常量：命令不接受前端传 URL，杜绝"打开任意链接"的口子。
pub const REPO: &str = "qq296565302/termharbor";
pub const RELEASES_PAGE: &str = "https://github.com/qq296565302/termharbor/releases/latest";

const API_LATEST: &str = "https://api.github.com/repos/qq296565302/termharbor/releases/latest";

/// 10 秒。GitHub 在国内经常连不上，等太久不如早点给一句能看懂的失败。
const TIMEOUT: Duration = Duration::from_secs(10);

/// 一个版本号，只取 major.minor.patch。
///
/// 预发布后缀（`-beta.1`）与构建元数据（`+abc`）在比较时忽略：这一版没有预发布通道，
/// 把它们算进来只会让"哪个更大"变成一个没人能记住的规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim().trim_start_matches('v');
        let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);

        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;

        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 在 `latest` 比 `current` 新时返回 true。
///
/// **必须按数字比，不能按字符串比**：`0.1.9` 字符串上大于 `0.1.15`，按字符串比
/// 会让用户永远收不到 0.1.10 之后的版本。
pub fn is_newer(latest: &Version, current: &Version) -> bool {
    latest > current
}

#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    published_at: Option<String>,
}

/// Releases 上的最新一版。`None` 表示还没有发过任何 release（不是错误）。
#[derive(Debug, Clone)]
pub struct Latest {
    pub version: Version,
    pub tag: String,
    pub published_at: Option<String>,
}

/// 拉一次 GitHub 的 `/releases/latest`。
///
/// 404 单独当成「还没发过版本」而不是失败：仓库刚建好、或者一直手发 exe 的情况下，
/// 用户点检查更新不该看到一条红色报错。
pub async fn fetch_latest() -> AppResult<Option<Latest>> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("TermHarbor/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| AppError::with_detail(ErrCode::UpdateCheckFailed, err.to_string()))?;

    let response = client
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| AppError::with_detail(ErrCode::UpdateCheckFailed, err.to_string()))?;

    match response.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        status => {
            return Err(AppError::with_detail(
                ErrCode::UpdateCheckFailed,
                format!("GitHub 返回 {status}"),
            ))
        }
    }

    let raw: RawRelease = response
        .json()
        .await
        .map_err(|err| AppError::with_detail(ErrCode::UpdateCheckFailed, err.to_string()))?;

    let Some(version) = Version::parse(&raw.tag_name) else {
        return Err(AppError::with_detail(
            ErrCode::UpdateCheckFailed,
            format!("看不懂的 tag 名：{}", raw.tag_name),
        ));
    };

    Ok(Some(Latest {
        version,
        tag: raw.tag_name,
        published_at: raw.published_at,
    }))
}

/// 用系统默认浏览器打开下载页。
///
/// 地址是常量，不接受参数 —— 这条路径只为了"点一下去下载"，不需要通用 open_url。
pub fn open_releases_page() -> AppResult<()> {
    let result = open_url(RELEASES_PAGE);
    result.map_err(|err| AppError::with_detail(ErrCode::UpdateCheckFailed, err))
}

#[cfg(windows)]
fn open_url(url: &str) -> Result<(), String> {
    // 空字符串是 start 命令的"窗口标题"占位，少了它，含空格/引号的 URL 会被解析歪
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("打开浏览器失败: {err}"))
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), String> {
    Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("打开浏览器失败: {err}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_url(url: &str) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("打开浏览器失败: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_prefixed_versions() {
        assert_eq!(
            Version::parse("0.1.15"),
            Some(Version {
                major: 0,
                minor: 1,
                patch: 15
            })
        );
        assert_eq!(Version::parse("v1.2.3"), Version::parse("1.2.3"));
        assert_eq!(Version::parse(" 0.1.15 "), Version::parse("0.1.15"));
    }

    #[test]
    fn ignores_prerelease_and_build_suffix() {
        let base = Version::parse("1.4.0").unwrap();
        assert_eq!(Version::parse("1.4.0-beta.2"), Some(base));
        assert_eq!(Version::parse("1.4.0+build.7"), Some(base));
    }

    #[test]
    fn missing_patch_is_zero() {
        assert_eq!(Version::parse("1.4"), Version::parse("1.4.0"));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("nightly"), None);
        assert_eq!(Version::parse("v1.x.3"), None);
    }

    /// 这条是重点：按字符串比会把 0.1.9 判成比 0.1.15 新，用户永远收不到更新。
    #[test]
    fn compares_numerically_not_lexically() {
        let older = Version::parse("0.1.9").unwrap();
        let newer = Version::parse("0.1.15").unwrap();
        assert!(is_newer(&newer, &older));
        assert!(!is_newer(&older, &newer));
        assert!("0.1.15" < "0.1.9", "字符串比较确实是反的，别改回字符串");
    }

    #[test]
    fn equal_versions_are_not_updates() {
        let v = Version::parse("0.1.15").unwrap();
        assert!(!is_newer(&v, &v));
    }

    #[test]
    fn major_and_minor_beat_patch() {
        let current = Version::parse("0.9.99").unwrap();
        assert!(is_newer(&Version::parse("1.0.0").unwrap(), &current));
        assert!(is_newer(&Version::parse("0.10.0").unwrap(), &current));
    }

    #[test]
    fn release_page_is_this_repo() {
        assert!(RELEASES_PAGE.contains(REPO));
        assert!(RELEASES_PAGE.starts_with("https://github.com/"));
    }
}
