//! 对象存储会话与工作区偏好。

use std::collections::HashSet;

use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_ACCOUNTS, MAX_OBJECT_STORAGE_SESSION_BYTES,
    MAX_OBJECT_STORAGE_WORKSPACE_BYTES, MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES,
    ObjectStorageAccountId, ObjectStorageSessionPreference, ObjectStorageWorkspacePreference,
    is_opendal_safe_prefix,
};
use ramag_domain::error::{DomainError, Result};

use super::{
    ENCRYPTED_WORKSPACE_PREFIX, MAX_ENCRYPTED_SESSION_BYTES, MAX_ENCRYPTED_WORKSPACE_BYTES,
    ObjectStorageService, SESSION_PREFERENCE_KEY, log_object_storage_error,
    workspace_preference_key,
};

impl ObjectStorageService {
    pub async fn load_session_preference(&self) -> Result<ObjectStorageSessionPreference> {
        let result = async {
            let Some(stored) = self.storage.get_preference(SESSION_PREFERENCE_KEY).await? else {
                return Ok(ObjectStorageSessionPreference::default());
            };
            if stored.len() > MAX_ENCRYPTED_SESSION_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储会话密文超过安全上限".into(),
                ));
            }
            let encoded = stored
                .strip_prefix(ENCRYPTED_WORKSPACE_PREFIX)
                .ok_or_else(|| DomainError::Storage("对象存储会话不是受支持的加密格式".into()))?;
            let encrypted = hex::decode(encoded)
                .map_err(|error| DomainError::Storage(format!("对象存储会话密文无效：{error}")))?;
            let plain = self.storage.unseal(&encrypted).await?;
            if plain.len() > MAX_OBJECT_STORAGE_SESSION_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储会话明文超过安全上限".into(),
                ));
            }
            let preference: ObjectStorageSessionPreference = serde_json::from_slice(&plain)
                .map_err(|error| DomainError::Storage(format!("对象存储会话格式无效：{error}")))?;
            validate_session_preference(&preference)?;
            Ok(preference)
        }
        .await;
        log_object_storage_error("object_storage_session_load", None, &result);
        result
    }

    pub async fn save_session_preference(
        &self,
        preference: &ObjectStorageSessionPreference,
    ) -> Result<()> {
        let result = async {
            validate_session_preference(preference)?;
            let plain = serde_json::to_vec(preference).map_err(|error| {
                DomainError::Storage(format!("序列化对象存储会话失败：{error}"))
            })?;
            if plain.len() > MAX_OBJECT_STORAGE_SESSION_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储会话明文超过安全上限".into(),
                ));
            }
            let encrypted = self.storage.seal(&plain).await?;
            let stored = format!("{ENCRYPTED_WORKSPACE_PREFIX}{}", hex::encode(encrypted));
            if stored.len() > MAX_ENCRYPTED_SESSION_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储会话密文超过安全上限".into(),
                ));
            }
            self.storage
                .set_preference(SESSION_PREFERENCE_KEY, &stored)
                .await
        }
        .await;
        log_object_storage_error("object_storage_session_save", None, &result);
        result
    }

    pub async fn load_workspace(
        &self,
        account_id: &ObjectStorageAccountId,
    ) -> Result<ObjectStorageWorkspacePreference> {
        let result = async {
            let _guard = self.account_gate(account_id).read_owned().await;
            if self
                .storage
                .get_object_storage_account(account_id)
                .await?
                .is_none()
            {
                return Err(DomainError::NotFound(format!("对象存储账号 {account_id}")));
            }
            let Some(stored) = self
                .storage
                .get_preference(&workspace_preference_key(account_id))
                .await?
            else {
                return Ok(ObjectStorageWorkspacePreference::default());
            };
            if stored.len() > MAX_ENCRYPTED_WORKSPACE_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储工作区密文超过安全上限".into(),
                ));
            }
            let encoded = stored
                .strip_prefix(ENCRYPTED_WORKSPACE_PREFIX)
                .ok_or_else(|| DomainError::Storage("对象存储工作区不是受支持的加密格式".into()))?;
            let encrypted = hex::decode(encoded).map_err(|error| {
                DomainError::Storage(format!("对象存储工作区密文无效：{error}"))
            })?;
            let plain = self.storage.unseal(&encrypted).await?;
            if plain.len() > MAX_OBJECT_STORAGE_WORKSPACE_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储工作区明文超过安全上限".into(),
                ));
            }
            let preference: ObjectStorageWorkspacePreference = serde_json::from_slice(&plain)
                .map_err(|error| {
                    DomainError::Storage(format!("对象存储工作区格式无效：{error}"))
                })?;
            validate_workspace(account_id, &preference)?;
            Ok(preference)
        }
        .await;
        log_object_storage_error("object_storage_workspace_load", Some(account_id), &result);
        result
    }

    pub async fn save_workspace(
        &self,
        account_id: &ObjectStorageAccountId,
        preference: &ObjectStorageWorkspacePreference,
    ) -> Result<()> {
        let result = async {
            validate_workspace(account_id, preference)?;
            let _guard = self.account_gate(account_id).read_owned().await;
            if self
                .storage
                .get_object_storage_account(account_id)
                .await?
                .is_none()
            {
                return Err(DomainError::NotFound(format!("对象存储账号 {account_id}")));
            }
            let plain = serde_json::to_vec(preference).map_err(|error| {
                DomainError::Storage(format!("序列化对象存储工作区失败：{error}"))
            })?;
            if plain.len() > MAX_OBJECT_STORAGE_WORKSPACE_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储工作区明文超过安全上限".into(),
                ));
            }
            let encrypted = self.storage.seal(&plain).await?;
            let stored = format!("{ENCRYPTED_WORKSPACE_PREFIX}{}", hex::encode(encrypted));
            if stored.len() > MAX_ENCRYPTED_WORKSPACE_BYTES {
                return Err(DomainError::InvalidConfig(
                    "对象存储工作区密文超过安全上限".into(),
                ));
            }
            self.storage
                .set_preference(&workspace_preference_key(account_id), &stored)
                .await
        }
        .await;
        log_object_storage_error("object_storage_workspace_save", Some(account_id), &result);
        result
    }
}

fn validate_session_preference(preference: &ObjectStorageSessionPreference) -> Result<()> {
    if preference.open_account_ids.len() > MAX_OBJECT_STORAGE_ACCOUNTS {
        return Err(DomainError::InvalidConfig(
            "已打开的对象存储账号超过安全上限".into(),
        ));
    }
    let unique: HashSet<_> = preference.open_account_ids.iter().collect();
    if unique.len() != preference.open_account_ids.len() {
        return Err(DomainError::InvalidConfig(
            "对象存储会话包含重复账号".into(),
        ));
    }
    if preference
        .active_account_id
        .as_ref()
        .is_some_and(|id| !unique.contains(id))
    {
        return Err(DomainError::InvalidConfig(
            "当前对象存储账号不在已打开会话中".into(),
        ));
    }
    Ok(())
}

fn validate_workspace(
    account_id: &ObjectStorageAccountId,
    preference: &ObjectStorageWorkspacePreference,
) -> Result<()> {
    if preference
        .workspaces
        .len()
        .saturating_add(preference.favorites.len())
        > MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES
    {
        return Err(DomainError::InvalidConfig(
            "对象存储工作区条目超过安全上限".into(),
        ));
    }
    if preference
        .active_account_id
        .as_ref()
        .is_some_and(|id| id != account_id)
        || preference
            .workspaces
            .iter()
            .any(|workspace| &workspace.account_id != account_id)
        || preference
            .favorites
            .iter()
            .any(|favorite| &favorite.account_id != account_id)
    {
        return Err(DomainError::InvalidConfig(
            "对象存储工作区包含其他账号的数据".into(),
        ));
    }
    if preference
        .workspaces
        .iter()
        .any(|workspace| !is_opendal_safe_prefix(&workspace.prefix))
        || preference
            .favorites
            .iter()
            .any(|favorite| !is_opendal_safe_prefix(&favorite.prefix))
    {
        return Err(DomainError::InvalidConfig(
            "对象存储工作区包含不安全前缀".into(),
        ));
    }
    let mut favorites = HashSet::new();
    for favorite in &preference.favorites {
        if !favorites.insert((favorite.mount_id.clone(), favorite.prefix.as_str())) {
            return Err(DomainError::InvalidConfig(
                "对象存储收藏夹存在重复条目".into(),
            ));
        }
    }
    Ok(())
}
