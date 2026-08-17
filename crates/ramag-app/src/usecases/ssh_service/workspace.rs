//! SSH 工作区偏好的加密持久化。

use super::*;
use crate::usecases::ssh_service::helpers::{
    normalized_workspace_preference, parse_workspace_preference,
};

const WORKSPACE_PREFERENCE_KEY: &str = "ssh_workspaces_v1";
pub(super) const MAX_WORKSPACE_PREFERENCE_BYTES: usize = 64 * 1024;
const MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES: usize = MAX_WORKSPACE_PREFERENCE_BYTES * 2 + 1024;
const ENCRYPTED_WORKSPACE_PREFIX: &str = "enc-v1:";

impl SshService {
    pub async fn load_workspace_preference(&self) -> Result<SshWorkspacePreference> {
        let result = async {
            let Some(stored) = self
                .storage
                .get_preference(WORKSPACE_PREFERENCE_KEY)
                .await?
            else {
                return Ok(SshWorkspacePreference::default());
            };
            if stored.len() > MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES {
                return Err(DomainError::InvalidConfig("SSH 工作区恢复数据过大".into()));
            }
            let json = if let Some(encoded) = stored.strip_prefix(ENCRYPTED_WORKSPACE_PREFIX) {
                let encrypted = hex::decode(encoded).map_err(|error| {
                    DomainError::Storage(format!("SSH 工作区密文编码无效：{error}"))
                })?;
                let plain = self.storage.unseal(&encrypted).await?;
                String::from_utf8(plain).map_err(|error| {
                    DomainError::Storage(format!("SSH 工作区解密结果不是 UTF-8：{error}"))
                })?
            } else {
                // 兼容旧明文；下次保存迁移为密文。
                stored
            };
            parse_workspace_preference(&json)
        }
        .await;
        match &result {
            Ok(preference) => tracing::debug!(
                operation = "ssh_workspace_preference_load",
                workspaces = preference.workspaces.len(),
                favorites = preference.path_favorites.len(),
                "ssh workspace preference loaded"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_workspace_preference_load",
                error = %error,
                "load ssh workspace preference failed"
            ),
        }
        result
    }

    pub async fn save_workspace_preference(
        &self,
        preference: &SshWorkspacePreference,
    ) -> Result<()> {
        let result = async {
            let preference = normalized_workspace_preference(preference.clone())?;
            let json = serde_json::to_string(&preference)
                .map_err(|error| DomainError::Storage(format!("序列化 SSH 工作区失败：{error}")))?;
            if json.len() > MAX_WORKSPACE_PREFERENCE_BYTES {
                return Err(DomainError::InvalidConfig("SSH 工作区恢复数据过大".into()));
            }
            let encrypted = self.storage.seal(json.as_bytes()).await?;
            let stored = format!("{ENCRYPTED_WORKSPACE_PREFIX}{}", hex::encode(encrypted));
            if stored.len() > MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES {
                return Err(DomainError::InvalidConfig(
                    "加密后的 SSH 工作区恢复数据过大".into(),
                ));
            }
            self.storage
                .set_preference(WORKSPACE_PREFERENCE_KEY, &stored)
                .await
        }
        .await;
        match &result {
            Ok(()) => tracing::debug!(
                operation = "ssh_workspace_preference_save",
                workspaces = preference.workspaces.len(),
                favorites = preference.path_favorites.len(),
                "ssh workspace preference saved"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_workspace_preference_save",
                error = %error,
                workspaces = preference.workspaces.len(),
                favorites = preference.path_favorites.len(),
                "save ssh workspace preference failed"
            ),
        }
        result
    }
}
