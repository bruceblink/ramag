//! 数据库结果表显示设置与 GPUI 全局状态。

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

pub const DATABASE_RESULT_SETTINGS_PREF_KEY: &str = "database_result_settings";

fn default_show_horizontal_scrollbar() -> bool {
    true
}

fn default_display_binary_16_as_uuid() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseResultSettings {
    /// 显示数据库结果表底部的水平滚动条；关闭后仍可通过横向手势滚动结果。
    #[serde(default = "default_show_horizontal_scrollbar")]
    pub show_horizontal_scrollbar: bool,
    /// 将 16 字节二进制值显示为 UUID；关闭后显示原始十六进制字符串。
    #[serde(default = "default_display_binary_16_as_uuid")]
    pub display_binary_16_as_uuid: bool,
}

impl Default for DatabaseResultSettings {
    fn default() -> Self {
        Self {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        }
    }
}

impl DatabaseResultSettings {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(raw).map_err(|error| format!("数据库结果表设置格式无效：{error}"))
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化数据库结果表设置失败：{error}"))
    }
}

pub struct DatabaseResultSettingsGlobal(DatabaseResultSettings);
impl Global for DatabaseResultSettingsGlobal {}

impl DatabaseResultSettingsGlobal {
    pub fn new(settings: DatabaseResultSettings) -> Self {
        Self(settings)
    }
}

pub fn database_result_settings(cx: &App) -> DatabaseResultSettings {
    cx.try_global::<DatabaseResultSettingsGlobal>()
        .map(|global| global.0)
        .unwrap_or_default()
}

pub fn set_database_result_settings(settings: DatabaseResultSettings, cx: &mut App) {
    cx.set_global(DatabaseResultSettingsGlobal(settings));
}

pub fn init_database_result_settings(preference: Option<&str>, cx: &mut App) -> Result<(), String> {
    match preference.map(DatabaseResultSettings::parse).transpose() {
        Ok(settings) => {
            set_database_result_settings(settings.unwrap_or_default(), cx);
            Ok(())
        }
        Err(error) => {
            set_database_result_settings(DatabaseResultSettings::default(), cx);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_setting_keeps_horizontal_scrollbar_enabled() -> Result<(), String> {
        assert_eq!(
            DatabaseResultSettings::parse("{}")?,
            DatabaseResultSettings::default()
        );
        assert!(DatabaseResultSettings::default().show_horizontal_scrollbar);
        assert!(DatabaseResultSettings::default().display_binary_16_as_uuid);
        Ok(())
    }

    #[test]
    fn legacy_setting_defaults_to_uuid_display() -> Result<(), String> {
        let settings = DatabaseResultSettings::parse(r#"{"show_horizontal_scrollbar":false}"#)?;
        assert!(!settings.show_horizontal_scrollbar);
        assert!(settings.display_binary_16_as_uuid);
        Ok(())
    }

    #[test]
    fn disabled_setting_round_trips() -> Result<(), String> {
        let settings = DatabaseResultSettings {
            show_horizontal_scrollbar: false,
            display_binary_16_as_uuid: false,
        };
        assert_eq!(
            DatabaseResultSettings::parse(&settings.to_json()?)?,
            settings
        );
        Ok(())
    }
}
