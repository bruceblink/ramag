//! 数据库结果搜索的全局设置与 GPUI 状态。

use std::path::Path;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

pub const DATABASE_SEARCH_SETTINGS_PREF_KEY: &str = "database_search_settings";
pub const MAX_ID_CONVERTER_PROGRAM_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseSearchSettings {
    /// 开启后，SQL 结果行搜索可选择 `@ID` 模式。
    #[serde(default)]
    pub id_conversion_enabled: bool,
    /// 外部转换程序的绝对路径；不经 shell 执行。
    #[serde(default)]
    pub id_converter_program: String,
}

impl DatabaseSearchSettings {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        let settings = serde_json::from_str::<Self>(raw)
            .map_err(|error| format!("数据库搜索设置格式无效：{error}"))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| format!("序列化数据库搜索设置失败：{error}"))
    }

    /// 这里只校验可持久化契约；文件存在性在用户保存时放到后台检查。
    pub fn validate(&self) -> Result<(), String> {
        if self.id_converter_program.len() > MAX_ID_CONVERTER_PROGRAM_BYTES {
            return Err(format!(
                "转换程序路径超过 {} KiB 上限",
                MAX_ID_CONVERTER_PROGRAM_BYTES / 1024
            ));
        }
        if self.id_converter_program.chars().any(char::is_control) {
            return Err("转换程序路径不能包含控制字符".to_string());
        }
        if self.id_conversion_enabled {
            if self.id_converter_program.is_empty() {
                return Err("开启 ID 转换前必须选择转换程序".to_string());
            }
            if !Path::new(&self.id_converter_program).is_absolute() {
                return Err("转换程序必须使用绝对路径".to_string());
            }
        }
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.id_conversion_enabled && !self.id_converter_program.is_empty()
    }
}

/// 保存设置时检查程序目标；执行时仍会再次由操作系统校验，覆盖文件被移动等情况。
pub fn validate_id_converter_program(program: &str) -> Result<(), String> {
    let path = Path::new(program);
    if !path.is_absolute() {
        return Err("转换程序必须使用绝对路径".to_string());
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("无法读取转换程序「{}」：{error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("转换程序不是普通文件：{}", path.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "转换程序没有执行权限，请先执行 chmod +x：{}",
                path.display()
            ));
        }
    }
    Ok(())
}

pub struct DatabaseSearchSettingsGlobal(DatabaseSearchSettings);
impl Global for DatabaseSearchSettingsGlobal {}

pub fn database_search_settings(cx: &App) -> DatabaseSearchSettings {
    cx.try_global::<DatabaseSearchSettingsGlobal>()
        .map(|global| global.0.clone())
        .unwrap_or_default()
}

pub fn set_database_search_settings(settings: DatabaseSearchSettings, cx: &mut App) {
    cx.set_global(DatabaseSearchSettingsGlobal(settings));
}

/// 初始化失败时回退关闭，避免损坏偏好静默启用外部程序。
pub fn init_database_search_settings(preference: Option<&str>, cx: &mut App) -> Result<(), String> {
    match preference.map(DatabaseSearchSettings::parse).transpose() {
        Ok(settings) => {
            set_database_search_settings(settings.unwrap_or_default(), cx);
            Ok(())
        }
        Err(error) => {
            set_database_search_settings(DatabaseSearchSettings::default(), cx);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_keep_conversion_disabled() -> Result<(), String> {
        let settings = DatabaseSearchSettings::parse("{}")?;

        assert_eq!(settings, DatabaseSearchSettings::default());
        assert!(!settings.is_ready());
        Ok(())
    }

    #[test]
    fn enabled_conversion_requires_an_absolute_program() {
        let missing = DatabaseSearchSettings {
            id_conversion_enabled: true,
            id_converter_program: String::new(),
        };
        assert!(missing.validate().is_err());

        let relative = DatabaseSearchSettings {
            id_conversion_enabled: true,
            id_converter_program: "converter".into(),
        };
        assert!(relative.validate().is_err());
    }

    #[test]
    fn disabled_conversion_may_remember_program_path() -> Result<(), String> {
        let settings = DatabaseSearchSettings {
            id_conversion_enabled: false,
            id_converter_program: "/opt/tools/id-converter".into(),
        };

        let encoded = settings.to_json()?;
        assert_eq!(DatabaseSearchSettings::parse(&encoded)?, settings);
        Ok(())
    }
}
