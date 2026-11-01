//! 检查更新的 command。
//!
//! 只做两件事：问一次 GitHub 有没有新版、以及打开下载页。不下载、不安装 ——
//! 原因写在 `crate::update` 的模块注释里（缺代码签名时静默安装会被拦）。

use serde::Serialize;

use crate::error::AppResult;
use crate::update::{self, Version};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// 当前运行版本，来自 tauri.conf.json（`app.package_info()`）
    pub current: String,
    /// 远端最新版本；仓库还没发过 release 时为 null
    pub latest: Option<String>,
    pub has_update: bool,
    pub published_at: Option<String>,
}

#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> AppResult<UpdateStatus> {
    let current_raw = app.package_info().version.to_string();
    let current = Version::parse(&current_raw);

    let latest = update::fetch_latest().await?;

    let (latest_version, published_at) = match &latest {
        Some(release) => (Some(release.version), release.published_at.clone()),
        None => (None, None),
    };

    // 当前版本号解析不出来时按"没有更新"处理：宁可什么都不提示，
    // 也不要因为版本号格式异常给用户弹一个说不清道不明的"有新版本"。
    let has_update = match (current, latest_version) {
        (Some(current), Some(latest)) => update::is_newer(&latest, &current),
        _ => false,
    };

    tracing::info!(
        current = %current_raw,
        latest = ?latest_version.map(|v| v.to_string()),
        has_update,
        "检查更新"
    );

    Ok(UpdateStatus {
        current: current_raw,
        latest: latest_version.map(|v| v.to_string()),
        has_update,
        published_at,
    })
}

/// 用系统浏览器打开本仓库的 releases 页面。地址写死在 Rust 侧，不接受前端传参。
#[tauri::command]
pub async fn update_open_page() -> AppResult<()> {
    update::open_releases_page()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前端按 `hasUpdate` 分支、按 `latest` 显示版本号，字段名是契约。
    #[test]
    fn status_serializes_with_camel_case() {
        let value = serde_json::to_value(UpdateStatus {
            current: "0.1.15".into(),
            latest: Some("0.1.16".into()),
            has_update: true,
            published_at: Some("2026-11-02T00:00:00Z".into()),
        })
        .unwrap();

        assert_eq!(value["current"], "0.1.15");
        assert_eq!(value["latest"], "0.1.16");
        assert_eq!(value["hasUpdate"], true);
        assert_eq!(value["publishedAt"], "2026-11-02T00:00:00Z");
    }

    /// 没发过 release 时是 `latest: null` + `hasUpdate: false`，不是错误。
    #[test]
    fn missing_release_is_not_an_update() {
        let value = serde_json::to_value(UpdateStatus {
            current: "0.1.15".into(),
            latest: None,
            has_update: false,
            published_at: None,
        })
        .unwrap();

        assert!(value["latest"].is_null());
        assert_eq!(value["hasUpdate"], false);
    }
}
