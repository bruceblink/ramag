//! Redis Key 树展示设置与 GPUI 全局状态。

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

pub const REDIS_TREE_SETTINGS_PREF_KEY: &str = "redis_tree_settings";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedisTreeSettings {
    /// 同一路径既是命名空间又是真实 Key 时，将真实 Key 放到子树末尾单独展示。
    #[serde(default)]
    pub sink_same_name_keys: bool,
}

impl RedisTreeSettings {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(raw).map_err(|error| format!("Redis Key 树设置格式无效：{error}"))
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化 Redis Key 树设置失败：{error}"))
    }
}

struct RedisTreeSettingsGlobal(RedisTreeSettings);
impl Global for RedisTreeSettingsGlobal {}

pub fn redis_tree_settings(cx: &App) -> RedisTreeSettings {
    cx.try_global::<RedisTreeSettingsGlobal>()
        .map(|global| global.0.clone())
        .unwrap_or_default()
}

pub fn set_redis_tree_settings(settings: RedisTreeSettings, cx: &mut App) {
    cx.set_global(RedisTreeSettingsGlobal(settings));
}

pub fn init_redis_tree_settings(preference: Option<&str>, cx: &mut App) -> Result<(), String> {
    match preference.map(RedisTreeSettings::parse).transpose() {
        Ok(settings) => {
            set_redis_tree_settings(settings.unwrap_or_default(), cx);
            Ok(())
        }
        Err(error) => {
            set_redis_tree_settings(RedisTreeSettings::default(), cx);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_setting_keeps_key_sinking_disabled() -> Result<(), String> {
        assert_eq!(
            RedisTreeSettings::parse("{}")?,
            RedisTreeSettings::default()
        );
        assert!(!RedisTreeSettings::default().sink_same_name_keys);
        Ok(())
    }

    #[test]
    fn enabled_setting_round_trips() -> Result<(), String> {
        let settings = RedisTreeSettings {
            sink_same_name_keys: true,
        };
        assert_eq!(RedisTreeSettings::parse(&settings.to_json()?)?, settings);
        Ok(())
    }
}
