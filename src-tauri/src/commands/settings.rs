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

/// 字号范围。下限保证 80 列还能塞进窗口，上限是"再大就只看得见几个字"。
const FONT_SIZE_MIN: u32 = 10;
const FONT_SIZE_MAX: u32 = 24;

/// 回滚行数范围。低于 1000 基本没法翻历史；高于 50000 内存占用开始明显。
const SCROLLBACK_MIN: u32 = 1_000;
const SCROLLBACK_MAX: u32 = 50_000;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub font_size: u32,
    pub theme: TerminalTheme,
    pub scrollback: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 13,
            theme: TerminalTheme::Dark,
            scrollback: 5_000,
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

        settings.sanitized()
    }

    fn to_pairs(&self) -> Vec<(String, String)> {
        vec![
            (KEY_FONT_SIZE.to_string(), self.font_size.to_string()),
            (KEY_THEME.to_string(), self.theme.as_str().to_string()),
            (KEY_SCROLLBACK.to_string(), self.scrollback.to_string()),
        ]
    }

    /// 把越界值收回到合法范围。
    fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        self.scrollback = self.scrollback.clamp(SCROLLBACK_MIN, SCROLLBACK_MAX);
        self
    }
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
        };
        let restored = Settings::from_pairs(&original.to_pairs().into_iter().collect());
        assert_eq!(restored, original);
    }
}
