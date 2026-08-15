//! 数据库结果搜索的全局设置与 GPUI 状态。

use std::path::Path;

use gpui::{App, Global};
use ramag_domain::entities::{IdConverterConfig, IdConverterKind};
use serde::{Deserialize, Deserializer, Serialize};

pub const DATABASE_SEARCH_SETTINGS_PREF_KEY: &str = "database_search_settings";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DatabaseSearchSettings {
    /// 开启后，数据库结果行搜索可选择双向 ID 转换模式。
    #[serde(default)]
    pub id_conversion_enabled: bool,
    #[serde(flatten)]
    pub converter: IdConverterConfig,
}

#[derive(Deserialize)]
struct DatabaseSearchSettingsWire {
    #[serde(default)]
    id_conversion_enabled: bool,
    /// 旧版配置没有此字段；存在旧程序路径时迁移为外部程序模式。
    #[serde(default)]
    id_converter_kind: Option<IdConverterKind>,
    #[serde(default)]
    id_converter_custom_alphabet: String,
    #[serde(default)]
    id_converter_program: String,
}

impl From<DatabaseSearchSettingsWire> for DatabaseSearchSettings {
    fn from(wire: DatabaseSearchSettingsWire) -> Self {
        let kind = wire.id_converter_kind.unwrap_or_else(|| {
            if wire.id_conversion_enabled || !wire.id_converter_program.is_empty() {
                IdConverterKind::ExternalProgram
            } else {
                IdConverterKind::default()
            }
        });
        Self {
            id_conversion_enabled: wire.id_conversion_enabled,
            converter: IdConverterConfig {
                kind,
                custom_alphabet: wire.id_converter_custom_alphabet,
                external_program: wire.id_converter_program,
            },
        }
    }
}

impl<'de> Deserialize<'de> for DatabaseSearchSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        DatabaseSearchSettingsWire::deserialize(deserializer).map(Into::into)
    }
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

    /// 仅校验可持久化契约；文件存在性在用户保存时放到后台检查。
    pub fn validate(&self) -> Result<(), String> {
        self.converter.validate_storable()?;
        if self.id_conversion_enabled {
            self.converter.validate_active()?;
        }
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.id_conversion_enabled && self.converter.validate_active().is_ok()
    }
}

/// 保存设置时检查外部程序目标；执行时仍会再次由操作系统校验。
pub fn validate_id_converter_program(program: &str) -> Result<(), String> {
    let path = Path::new(program);
    if !path.is_absolute() {
        return Err("ID 转换器必须使用绝对路径".to_string());
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("无法读取 ID 转换器「{}」：{error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("ID 转换器不是普通文件：{}", path.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "ID 转换器没有执行权限，请先执行 chmod +x：{}",
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
    fn missing_fields_keep_conversion_disabled_with_flickr_base58() -> Result<(), String> {
        let settings = DatabaseSearchSettings::parse("{}")?;

        assert_eq!(settings, DatabaseSearchSettings::default());
        assert_eq!(settings.converter.kind, IdConverterKind::Base58Flickr);
        assert_eq!(settings.converter.decode_local("qwe")?, 82_489);
        assert!(!settings.is_ready());
        Ok(())
    }

    #[test]
    fn legacy_program_setting_migrates_to_external_program() -> Result<(), String> {
        let program = std::env::temp_dir().join("ramag-id-converter");
        let raw = serde_json::json!({
            "id_conversion_enabled": true,
            "id_converter_program": program,
        })
        .to_string();
        let settings = DatabaseSearchSettings::parse(&raw)?;

        assert_eq!(settings.converter.kind, IdConverterKind::ExternalProgram);
        assert_eq!(
            settings.converter.external_program,
            program.to_string_lossy()
        );
        assert!(settings.is_ready());
        Ok(())
    }

    #[test]
    fn invalid_legacy_enabled_setting_does_not_silently_enable_a_builtin() {
        let result = DatabaseSearchSettings::parse(r#"{"id_conversion_enabled":true}"#);

        assert!(result.is_err());
    }

    #[test]
    fn enabled_custom_conversion_requires_a_valid_alphabet() {
        let missing = DatabaseSearchSettings {
            id_conversion_enabled: true,
            converter: IdConverterConfig {
                kind: IdConverterKind::CustomAlphabet,
                ..IdConverterConfig::default()
            },
        };
        assert!(missing.validate().is_err());

        let duplicate = DatabaseSearchSettings {
            id_conversion_enabled: true,
            converter: IdConverterConfig {
                kind: IdConverterKind::CustomAlphabet,
                custom_alphabet: "aab".into(),
                ..IdConverterConfig::default()
            },
        };
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn disabled_conversion_may_remember_inactive_fields() -> Result<(), String> {
        let settings = DatabaseSearchSettings {
            id_conversion_enabled: false,
            converter: IdConverterConfig {
                kind: IdConverterKind::ExternalProgram,
                custom_alphabet: "尚未完成 aa".into(),
                external_program: "/opt/tools/id-converter".into(),
            },
        };

        let encoded = settings.to_json()?;
        assert_eq!(DatabaseSearchSettings::parse(&encoded)?, settings);
        Ok(())
    }
}
