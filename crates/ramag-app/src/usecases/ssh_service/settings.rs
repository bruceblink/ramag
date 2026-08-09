//! SSH 模块级设置的持久化与运行时应用。

use super::*;

const SSH_MODULE_SETTINGS_KEY: &str = "ssh_module_settings_v1";
const MAX_SSH_MODULE_SETTINGS_BYTES: usize = 4 * 1024;

impl SshService {
    pub fn module_settings_snapshot(&self) -> SshModuleSettings {
        *self.module_settings.lock()
    }

    pub async fn load_module_settings(&self) -> Result<SshModuleSettings> {
        self.ensure_module_settings_loaded().await?;
        Ok(self.module_settings_snapshot())
    }

    pub async fn save_module_settings(&self, settings: &SshModuleSettings) -> Result<()> {
        let _guard = self.module_settings_io.lock().await;
        let json = serde_json::to_string(settings)
            .map_err(|error| DomainError::Storage(format!("序列化 SSH 模块设置失败：{error}")))?;
        if json.len() > MAX_SSH_MODULE_SETTINGS_BYTES {
            return Err(DomainError::InvalidConfig("SSH 模块设置数据过大".into()));
        }
        self.storage
            .set_preference(SSH_MODULE_SETTINGS_KEY, &json)
            .await?;

        let changed = *self.module_settings.lock() != *settings;
        *self.module_settings.lock() = *settings;
        self.module_settings_loaded.store(true, Ordering::Release);
        if changed {
            self.remote_capabilities.lock().clear();
        }
        tracing::info!(
            operation = "ssh_module_settings_save",
            changed,
            windows_sftp_compatibility = settings.windows_sftp_compatibility,
            "ssh module settings saved"
        );
        Ok(())
    }

    pub(super) async fn ensure_module_settings_loaded(&self) -> Result<()> {
        if self.module_settings_loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self.module_settings_io.lock().await;
        if self.module_settings_loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let settings = match self.storage.get_preference(SSH_MODULE_SETTINGS_KEY).await? {
            Some(json) => {
                if json.len() > MAX_SSH_MODULE_SETTINGS_BYTES {
                    return Err(DomainError::InvalidConfig("SSH 模块设置数据过大".into()));
                }
                serde_json::from_str(&json).map_err(|error| {
                    DomainError::Storage(format!("解析 SSH 模块设置失败：{error}"))
                })?
            }
            None => SshModuleSettings::default(),
        };
        *self.module_settings.lock() = settings;
        self.module_settings_loaded.store(true, Ordering::Release);
        tracing::info!(
            operation = "ssh_module_settings_load",
            windows_sftp_compatibility = settings.windows_sftp_compatibility,
            "ssh module settings loaded"
        );
        Ok(())
    }

    pub(super) fn apply_module_settings(&self, profile: &SshProfile) -> SshProfile {
        let mut effective = profile.clone();
        effective.windows_sftp_compatibility =
            self.module_settings.lock().windows_sftp_compatibility
                && effective.remote_platform != RemotePlatformPreference::Linux;
        effective
    }
}
