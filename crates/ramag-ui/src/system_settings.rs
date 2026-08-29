//! 应用级系统设置与 GPUI 全局状态。

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

/// 系统设置在本地偏好存储中的键名。
pub const SYSTEM_SETTINGS_PREF_KEY: &str = "system_settings";

/// 应用级窗口行为设置。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSettings {
    /// Windows 上关闭主窗口后是否保留进程并由任务栏托盘重新打开。
    #[serde(default)]
    pub minimize_to_tray: bool,
}

impl SystemSettings {
    /// 解析本地偏好；空值表示尚未保存，格式错误交给启动逻辑回退为默认值。
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(raw).map_err(|error| format!("系统设置格式无效：{error}"))
    }

    /// 将系统设置编码为可写入本地偏好存储的 JSON。
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化系统设置失败：{error}"))
    }
}

pub struct SystemSettingsGlobal(SystemSettings);
impl Global for SystemSettingsGlobal {}

/// 从 GPUI 全局状态读取系统设置；启动前未初始化时保持默认关闭。
pub fn system_settings(cx: &App) -> SystemSettings {
    cx.try_global::<SystemSettingsGlobal>()
        .map(|global| global.0)
        .unwrap_or_default()
}

/// 更新 GPUI 全局状态，供窗口生命周期和设置页面共享最新值。
pub fn set_system_settings(settings: SystemSettings, cx: &mut App) {
    cx.set_global(SystemSettingsGlobal(settings));
}

/// 初始化系统设置；损坏配置不会意外启用后台驻留。
pub fn init_system_settings(preference: Option<&str>, cx: &mut App) -> Result<(), String> {
    match preference.map(SystemSettings::parse).transpose() {
        Ok(settings) => {
            set_system_settings(settings.unwrap_or_default(), cx);
            Ok(())
        }
        Err(error) => {
            set_system_settings(SystemSettings::default(), cx);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_setting_keeps_tray_minimization_disabled() -> Result<(), String> {
        assert_eq!(SystemSettings::parse("{}")?, SystemSettings::default());
        assert!(!SystemSettings::default().minimize_to_tray);
        Ok(())
    }

    #[test]
    fn enabled_setting_round_trips() -> Result<(), String> {
        let settings = SystemSettings {
            minimize_to_tray: true,
        };
        assert_eq!(SystemSettings::parse(&settings.to_json()?)?, settings);
        Ok(())
    }

    #[test]
    fn invalid_setting_is_rejected() {
        assert!(SystemSettings::parse(r#"{"minimize_to_tray":"yes"}"#).is_err());
    }
}
