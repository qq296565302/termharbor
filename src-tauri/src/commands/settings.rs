//! 设置项 command。
//!
//! 三个关键决定：
//!
//! 1. **越界值静默 clamp，不报错**。用户拖滑块到边界之外（或改了配置文件）时，
//!    弹一个「输入不合法」既没必要也打断操作，直接修正到合法范围更友好。
//! 2. **缺失的键用默认值补齐**，而不是当错误。老版本升级上来时表里就那几行。
//! 3. **写入是全量覆盖**：前端拿到的 Settings 结构改一个字段再整个提交，
//!    避免「部分更新」在键名上做文章。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;
use crate::storage::settings as store;

const KEY_FONT_SIZE: &str = "terminal.fontSize";
const KEY_THEME: &str = "terminal.theme";
const KEY_SCROLLBACK: &str = "terminal.scrollback";
/// 界面外观。和 `terminal.theme` 是两件事：那个只改终端配色，这个改整个界面。
const KEY_UI_THEME: &str = "ui.theme";
// 进阶功能设置（对标 FinalShell 高级版 / 专业版）。默认值刻意让功能开箱可见，
// 但都能在设置页关掉，不强制改变老用户习惯。
const KEY_BACKGROUND_IMAGE: &str = "terminal.backgroundImage";
const KEY_BACKGROUND_OPACITY: &str = "terminal.backgroundOpacity";
const KEY_COMMAND_BAR: &str = "terminal.commandBar";
const KEY_AUTO_PACK: &str = "transfer.autoPack";
const KEY_PACK_THRESHOLD: &str = "transfer.packThreshold";
const KEY_MONITOR_INTERFACES: &str = "monitor.interfaces";

/// 字号范围。下限保证 80 列还能塞进窗口，上限是"再大就只看得见几个字"。
const FONT_SIZE_MIN: u32 = 10;
const FONT_SIZE_MAX: u32 = 24;

/// 回滚行数范围。低于 1000 基本没法翻历史；高于 50000 内存占用开始明显。
const SCROLLBACK_MIN: u32 = 1_000;
const SCROLLBACK_MAX: u32 = 50_000;

/// 背景图不透明度（0–100）。默认压到 40：图片太实会盖住终端文字。
const OPACITY_MIN: u32 = 0;
const OPACITY_MAX: u32 = 100;

/// 触发「自动打包」的文件数阈值。1 = 任何多文件传输都打包。
const PACK_THRESHOLD_MIN: u32 = 1;
const PACK_THRESHOLD_MAX: u32 = 100;

/// 背景图上限 8MB。再大转成 data URL 会让 WebView 卡住，也没必要。
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalTheme {
    Dark,
    Light,
}

impl TerminalTheme {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// 界面外观。
///
/// 为什么不直接复用 `TerminalTheme`：终端配色是个人口味（白界面配黑终端很正常），
/// 界面明暗是整块 UI 的事，两者默认不该绑死。0.2.3 之前界面只跟系统走，
/// 用户想改只能去 Windows 设置里改「应用模式」——现在有开关了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    /// 跟随系统（默认）
    System,
    Light,
    Dark,
}

impl UiTheme {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub font_size: u32,
    pub theme: TerminalTheme,
    pub scrollback: u32,
    pub ui_theme: UiTheme,
    /// 终端背景图的本机绝对路径，空串 = 不显示。存路径不存图片本身：
    /// 一张几 MB 的 data URL 塞进 KV 表会让每次 settings_get 都拖一大坨。
    pub background_image: String,
    /// 背景图不透明度 0–100，数值越大图越实。
    pub background_opacity: u32,
    /// 是否显示终端下方的独立命令输入栏（快速路径 / 命令补全 / 历史字段选择）。
    pub command_bar: bool,
    /// 一次传多个文件时自动打成 tar.gz 再传、到对端自动解开。
    pub auto_pack: bool,
    /// 触发自动打包的文件数阈值。
    pub pack_threshold: u32,
    /// 负载看板要显示的网卡，逗号分隔；空串 = 全部。纯界面偏好。
    pub monitor_interfaces: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 13,
            theme: TerminalTheme::Dark,
            scrollback: 5_000,
            ui_theme: UiTheme::System,
            background_image: String::new(),
            background_opacity: 40,
            command_bar: true,
            auto_pack: true,
            pack_threshold: 5,
            monitor_interfaces: String::new(),
        }
    }
}

impl Settings {
    /// 从 KV 组装。任何缺失或无法解析的项都退回默认值。
    fn from_pairs(pairs: &HashMap<String, String>) -> Self {
        let mut settings = Self::default();

        if let Some(value) = pairs.get(KEY_FONT_SIZE).and_then(|raw| raw.parse().ok()) {
            settings.font_size = value;
        }
        if let Some(value) = pairs
            .get(KEY_THEME)
            .and_then(|raw| TerminalTheme::parse(raw))
        {
            settings.theme = value;
        }
        if let Some(value) = pairs.get(KEY_SCROLLBACK).and_then(|raw| raw.parse().ok()) {
            settings.scrollback = value;
        }
        // 老版本没有这个键，读不到就保持默认（跟随系统）——升级不该改变用户看到的外观。
        if let Some(value) = pairs.get(KEY_UI_THEME).and_then(|raw| UiTheme::parse(raw)) {
            settings.ui_theme = value;
        }
        // 新增项同样遵循「缺键即默认」：老版本升级上来不该突然多出一张背景图。
        if let Some(value) = pairs.get(KEY_BACKGROUND_IMAGE) {
            settings.background_image = value.clone();
        }
        if let Some(value) = pairs
            .get(KEY_BACKGROUND_OPACITY)
            .and_then(|raw| raw.parse().ok())
        {
            settings.background_opacity = value;
        }
        if let Some(value) = pairs.get(KEY_COMMAND_BAR).and_then(|raw| parse_bool(raw)) {
            settings.command_bar = value;
        }
        if let Some(value) = pairs.get(KEY_AUTO_PACK).and_then(|raw| parse_bool(raw)) {
            settings.auto_pack = value;
        }
        if let Some(value) = pairs
            .get(KEY_PACK_THRESHOLD)
            .and_then(|raw| raw.parse().ok())
        {
            settings.pack_threshold = value;
        }
        if let Some(value) = pairs.get(KEY_MONITOR_INTERFACES) {
            settings.monitor_interfaces = value.clone();
        }

        settings.sanitized()
    }

    fn to_pairs(&self) -> Vec<(String, String)> {
        vec![
            (KEY_FONT_SIZE.to_string(), self.font_size.to_string()),
            (KEY_THEME.to_string(), self.theme.as_str().to_string()),
            (KEY_SCROLLBACK.to_string(), self.scrollback.to_string()),
            (KEY_UI_THEME.to_string(), self.ui_theme.as_str().to_string()),
            (
                KEY_BACKGROUND_IMAGE.to_string(),
                self.background_image.clone(),
            ),
            (
                KEY_BACKGROUND_OPACITY.to_string(),
                self.background_opacity.to_string(),
            ),
            (KEY_COMMAND_BAR.to_string(), self.command_bar.to_string()),
            (KEY_AUTO_PACK.to_string(), self.auto_pack.to_string()),
            (
                KEY_PACK_THRESHOLD.to_string(),
                self.pack_threshold.to_string(),
            ),
            (
                KEY_MONITOR_INTERFACES.to_string(),
                self.monitor_interfaces.clone(),
            ),
        ]
    }

    /// 把越界值收回到合法范围。字符串类也要限长：KV 表被人手改过之后
    /// 不该让一个几十 MB 的值一路传到 WebView。
    fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        self.scrollback = self.scrollback.clamp(SCROLLBACK_MIN, SCROLLBACK_MAX);
        self.background_opacity = self.background_opacity.clamp(OPACITY_MIN, OPACITY_MAX);
        self.pack_threshold = self
            .pack_threshold
            .clamp(PACK_THRESHOLD_MIN, PACK_THRESHOLD_MAX);
        self.background_image = truncate(&self.background_image, 1024);
        // 网卡名里不该出现控制字符，顺手滤掉，免得前端拼选择器时出问题
        self.monitor_interfaces = self
            .monitor_interfaces
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        self
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// 把本机图片读成 data URL 交给 WebView。
///
/// 为什么不开放 asset 协议：那要给整个文件系统开 scope，权限面比读一个用户
/// 自己挑中的图片大得多。这里只认用户通过文件对话框选出来的那一个文件，
/// 而且限扩展名、限大小，读进内存后立刻编码返回。
#[tauri::command]
pub async fn read_background_image(path: String) -> AppResult<String> {
    use base64::Engine;
    use std::path::Path;

    const ALLOWED: &[(&str, &str)] = &[
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
        ("bmp", "image/bmp"),
    ];

    let file = Path::new(&path);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = ALLOWED
        .iter()
        .find(|(name, _)| *name == ext)
        .map(|(_, mime)| *mime)
        .ok_or_else(|| {
            crate::error::AppError::validation("只支持 PNG / JPG / WebP / GIF / BMP 图片")
        })?;

    let meta = std::fs::metadata(file)
        .map_err(|_| crate::error::AppError::validation("找不到这张图片，可能已被移动或删除"))?;
    if !meta.is_file() {
        return Err(crate::error::AppError::validation("请选择一张图片文件"));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(crate::error::AppError::validation(
            "图片超过 8MB，请换一张小一点的",
        ));
    }

    // 读文件是同步 IO，但只有几 MB 且只在用户换背景图时跑一次，
    // 用 spawn_blocking 把它挪出 Tokio 工作线程，别卡住终端输出。
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|err| crate::error::AppError::internal(format!("读取图片失败: {err}")))?
        .map_err(|_| crate::error::AppError::validation("读取图片失败，请检查权限"))?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> AppResult<Settings> {
    let pairs = state.db.call(store::load_all).await?;
    Ok(Settings::from_pairs(&pairs))
}

#[tauri::command]
pub async fn settings_update(
    state: State<'_, AppState>,
    settings: Settings,
) -> AppResult<Settings> {
    let sanitized = settings.sanitized();
    let pairs = sanitized.to_pairs();

    state
        .db
        .call(move |conn| store::upsert_many(conn, &pairs))
        .await?;

    tracing::info!(
        font_size = sanitized.font_size,
        theme = sanitized.theme.as_str(),
        scrollback = sanitized.scrollback,
        "设置已保存"
    );

    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> HashMap<String, String> {
        items
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn empty_store_yields_defaults() {
        let settings = Settings::from_pairs(&HashMap::new());
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.font_size, 13);
        assert_eq!(settings.theme, TerminalTheme::Dark);
    }

    #[test]
    fn stored_values_are_read_back() {
        let settings = Settings::from_pairs(&pairs(&[
            (KEY_FONT_SIZE, "18"),
            (KEY_THEME, "light"),
            (KEY_SCROLLBACK, "12000"),
        ]));

        assert_eq!(settings.font_size, 18);
        assert_eq!(settings.theme, TerminalTheme::Light);
        assert_eq!(settings.scrollback, 12_000);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        // 用户把滑块拖到边界外、或者手动改了库，都该被修正而不是报错
        let tiny = Settings::from_pairs(&pairs(&[(KEY_FONT_SIZE, "2")]));
        assert_eq!(tiny.font_size, FONT_SIZE_MIN);

        let huge = Settings::from_pairs(&pairs(&[
            (KEY_FONT_SIZE, "999"),
            (KEY_SCROLLBACK, "99999999"),
        ]));
        assert_eq!(huge.font_size, FONT_SIZE_MAX);
        assert_eq!(huge.scrollback, SCROLLBACK_MAX);
    }

    /// 老版本的库里没有 `ui.theme`：必须退回「跟随系统」，不能因为缺键变成别的样子。
    #[test]
    fn missing_ui_theme_follows_the_system() {
        let settings = Settings::from_pairs(&pairs(&[(KEY_FONT_SIZE, "15")]));
        assert_eq!(settings.ui_theme, UiTheme::System);
    }

    #[test]
    fn ui_theme_round_trips() {
        for value in ["system", "light", "dark"] {
            let settings = Settings::from_pairs(&pairs(&[(KEY_UI_THEME, value)]));
            assert_eq!(settings.ui_theme.as_str(), value);
        }
        // 认不出来的值退回默认，而不是把界面变成未知状态
        let bogus = Settings::from_pairs(&pairs(&[(KEY_UI_THEME, "neon")]));
        assert_eq!(bogus.ui_theme, UiTheme::System);
    }

    /// 进阶设置缺键时全部走默认值：老库升级上来不能凭空多出背景图或关掉命令栏。
    #[test]
    fn missing_advanced_keys_use_defaults() {
        let settings = Settings::from_pairs(&pairs(&[(KEY_FONT_SIZE, "16")]));
        let defaults = Settings::default();
        assert_eq!(settings.background_image, defaults.background_image);
        assert_eq!(settings.background_opacity, defaults.background_opacity);
        assert_eq!(settings.command_bar, defaults.command_bar);
        assert_eq!(settings.auto_pack, defaults.auto_pack);
        assert_eq!(settings.pack_threshold, defaults.pack_threshold);
        assert_eq!(settings.monitor_interfaces, defaults.monitor_interfaces);
    }

    #[test]
    fn advanced_values_round_trip_and_clamp() {
        let settings = Settings::from_pairs(&pairs(&[
            (KEY_BACKGROUND_IMAGE, "D:/a.png"),
            (KEY_BACKGROUND_OPACITY, "999"),
            (KEY_COMMAND_BAR, "false"),
            (KEY_AUTO_PACK, "false"),
            (KEY_PACK_THRESHOLD, "0"),
            (KEY_MONITOR_INTERFACES, "eth0\n,eth1"),
        ]));
        assert_eq!(settings.background_image, "D:/a.png");
        assert_eq!(settings.background_opacity, OPACITY_MAX);
        assert!(!settings.command_bar);
        assert!(!settings.auto_pack);
        assert_eq!(settings.pack_threshold, PACK_THRESHOLD_MIN);
        assert_eq!(settings.monitor_interfaces, "eth0,eth1");
        // 布尔值写坏了要退回默认，不能把开关卡在未知状态
        let bogus = Settings::from_pairs(&pairs(&[(KEY_COMMAND_BAR, "yes")]));
        assert!(bogus.command_bar);
    }

    #[test]
    fn unparsable_values_fall_back_to_defaults() {
        let settings = Settings::from_pairs(&pairs(&[
            (KEY_FONT_SIZE, "big"),
            (KEY_THEME, "neon"),
            (KEY_SCROLLBACK, ""),
        ]));

        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn round_trip_through_pairs_is_lossless() {
        let original = Settings {
            font_size: 15,
            theme: TerminalTheme::Light,
            scrollback: 8_000,
            ui_theme: UiTheme::Dark,
            background_image: "C:/pics/wall.png".to_string(),
            background_opacity: 65,
            command_bar: false,
            auto_pack: false,
            pack_threshold: 3,
            monitor_interfaces: "eth0,eth1".to_string(),
        };
        let restored = Settings::from_pairs(&original.to_pairs().into_iter().collect());
        assert_eq!(restored, original);
    }
}
