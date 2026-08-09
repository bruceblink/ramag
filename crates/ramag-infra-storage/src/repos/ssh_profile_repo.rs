//! SSH 配置 CRUD。整条配置经主密钥加密，路径与主机信息不明文落盘。

use std::sync::Arc;

use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};
use tracing::{debug, info};

use ramag_domain::entities::{MAX_SSH_PROFILES, SshProfile, SshProfileId};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::bounded_json;

const MAX_SSH_PROFILE_RECORD_BYTES: usize = 1024 * 1024;
const MAX_SSH_PROFILE_LIST_BYTES: usize = 64 * 1024 * 1024;

/// 键为 Profile UUID，值为加密后的 Profile JSON hex。
pub(crate) const SSH_PROFILES_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("ssh_profiles");

fn encode_profile(profile: &SshProfile, cipher: &Cipher) -> Result<String> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    let json = bounded_json::serialize(profile, MAX_SSH_PROFILE_RECORD_BYTES, "SSH 配置")?;
    cipher.encrypt(&json)
}

fn decode_profile(key: &str, value: &str, cipher: &Cipher) -> Result<SshProfile> {
    bounded_json::ensure_len(
        value.len(),
        MAX_SSH_PROFILE_RECORD_BYTES * 2 + 64,
        &format!("SSH 配置 {key}"),
    )?;
    let json = cipher
        .decrypt(value)
        .map_err(|error| DomainError::Storage(format!("解密 SSH 配置 {key} 失败：{error}")))?;
    let profile: SshProfile = serde_json::from_str(&json)
        .map_err(|error| DomainError::Storage(format!("反序列化 SSH 配置 {key} 失败：{error}")))?;
    profile
        .validate()
        .map_err(|error| DomainError::Storage(format!("解密后的 SSH 配置 {key} 无效：{error}")))?;
    if profile.id.to_string() != key {
        return Err(DomainError::Storage(format!(
            "SSH 配置键与内容 ID 不一致：{key}"
        )));
    }
    Ok(profile)
}

pub(crate) fn list(db: Arc<Database>, cipher: Arc<RwLock<Cipher>>) -> Result<Vec<SshProfile>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(SSH_PROFILES_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 ssh_profiles 表失败：{error}")))?;
    let cipher = cipher.read();
    let mut profiles = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in table
        .iter()
        .map_err(|error| DomainError::Storage(format!("遍历 SSH 配置失败：{error}")))?
    {
        let (key, value) =
            entry.map_err(|error| DomainError::Storage(format!("读取 SSH 配置失败：{error}")))?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            profiles.len(),
            retained_bytes,
            value.value().len(),
            MAX_SSH_PROFILES,
            MAX_SSH_PROFILE_LIST_BYTES,
            "SSH 配置列表",
        )?;
        retained_bytes = next_bytes;
        profiles.push(decode_profile(key.value(), value.value(), &cipher)?);
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    debug!(
        operation = "ssh_profile_list",
        count = profiles.len(),
        "ssh profile listing completed"
    );
    Ok(profiles)
}

pub(crate) fn get(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    id: String,
) -> Result<Option<SshProfile>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(SSH_PROFILES_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 ssh_profiles 表失败：{error}")))?;
    let value = table
        .get(id.as_str())
        .map_err(|error| DomainError::Storage(format!("读取 SSH 配置 {id} 失败：{error}")))?;
    value
        .map(|value| decode_profile(&id, value.value(), &cipher.read()))
        .transpose()
}

pub(crate) fn save(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    profile: SshProfile,
) -> Result<()> {
    let value = encode_profile(&profile, &cipher.read())?;
    let id = profile.id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut table = write_txn
            .open_table(SSH_PROFILES_TABLE)
            .map_err(|error| DomainError::Storage(format!("打开 ssh_profiles 表失败：{error}")))?;
        let mut count = 0usize;
        let mut total_bytes = 0usize;
        let mut replaced_bytes = 0usize;
        for entry in table
            .iter()
            .map_err(|error| DomainError::Storage(format!("遍历 SSH 配置失败：{error}")))?
        {
            let (key, existing) = entry
                .map_err(|error| DomainError::Storage(format!("读取 SSH 配置失败：{error}")))?;
            (count, total_bytes) = bounded_json::next_collection_budget(
                count,
                total_bytes,
                existing.value().len(),
                MAX_SSH_PROFILES,
                MAX_SSH_PROFILE_LIST_BYTES,
                "SSH 配置列表",
            )?;
            if key.value() == id {
                replaced_bytes = existing.value().len();
            }
        }
        let final_count = count.saturating_add(usize::from(replaced_bytes == 0));
        let final_bytes = total_bytes
            .checked_sub(replaced_bytes)
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| DomainError::Storage("SSH 配置列表总大小溢出".into()))?;
        bounded_json::ensure_collection_budget(
            final_count,
            final_bytes,
            MAX_SSH_PROFILES,
            MAX_SSH_PROFILE_LIST_BYTES,
            "SSH 配置列表",
        )?;
        table
            .insert(id.as_str(), value.as_str())
            .map_err(|error| DomainError::Storage(format!("写入 SSH 配置 {id} 失败：{error}")))?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "ssh_profile_save", profile_id = %profile.id, "ssh profile saved");
    Ok(())
}

pub(crate) fn delete(db: Arc<Database>, id: SshProfileId) -> Result<()> {
    let id = id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut table = write_txn
            .open_table(SSH_PROFILES_TABLE)
            .map_err(|error| DomainError::Storage(format!("打开 ssh_profiles 表失败：{error}")))?;
        table
            .remove(id.as_str())
            .map_err(|error| DomainError::Storage(format!("删除 SSH 配置 {id} 失败：{error}")))?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "ssh_profile_delete", profile_id = %id, "ssh profile deleted");
    Ok(())
}

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(SSH_PROFILES_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开 ssh_profiles 表失败：{error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_record_does_not_contain_profile_fields() {
        let cipher = Cipher::new(&[9; 32]);
        let mut profile = SshProfile::new("production", "secret.example.com");
        profile.auth_mode = ramag_domain::entities::SshAuthMode::Password;
        profile.password = "secret-password".into();
        let encoded = encode_profile(&profile, &cipher).unwrap();

        assert!(!encoded.contains("production"));
        assert!(!encoded.contains("secret.example.com"));
        assert!(!encoded.contains("secret-password"));
        assert_eq!(
            decode_profile(&profile.id.to_string(), &encoded, &cipher).unwrap(),
            profile
        );
    }

    #[test]
    fn legacy_color_profile_migrates_to_environment_fields() {
        let cipher = Cipher::new(&[7; 32]);
        let profile = SshProfile::new("legacy", "server.example");
        let json = serde_json::json!({
            "id": profile.id.clone(),
            "name": "legacy",
            "color": "#007ACC",
            "host": "server.example",
            "port": 2222,
            "username": "deploy",
            "auth_mode": "System",
            "key_path": null,
            "initial_directory": null,
            "ssh_path": null
        })
        .to_string();
        let encoded = cipher.encrypt(&json).unwrap();

        let decoded = decode_profile(&profile.id.to_string(), &encoded, &cipher).unwrap();
        assert_eq!(decoded.environment, None);
        assert!(!decoded.production);
        assert_eq!(decoded.port, Some(2222));
        assert!(decoded.password.is_empty());
    }
}
