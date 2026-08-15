//! 云对象存储账号 CRUD。账号名称、AK/SK 与 Bucket 挂载点整条加密落盘。

use std::sync::Arc;

use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};
use tracing::{debug, info};

use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_ACCOUNTS, ObjectStorageAccount, ObjectStorageAccountId,
};
use ramag_domain::error::{DomainError, Result};

use crate::encryption::Cipher;
use crate::repos::{bounded_json, prefs_repo};

const MAX_ACCOUNT_RECORD_BYTES: usize = 1024 * 1024;
const MAX_ACCOUNT_LIST_BYTES: usize = 64 * 1024 * 1024;

pub(crate) const OBJECT_STORAGE_ACCOUNTS_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("object_storage_accounts");

fn encode(account: &ObjectStorageAccount, cipher: &Cipher) -> Result<String> {
    account.validate().map_err(DomainError::InvalidConfig)?;
    let json = bounded_json::serialize(account, MAX_ACCOUNT_RECORD_BYTES, "云存储账号")?;
    cipher.encrypt(&json)
}

fn decode(key: &str, value: &str, cipher: &Cipher) -> Result<ObjectStorageAccount> {
    bounded_json::ensure_len(
        value.len(),
        MAX_ACCOUNT_RECORD_BYTES * 2 + 64,
        &format!("云存储账号 {key}"),
    )?;
    let json = cipher
        .decrypt(value)
        .map_err(|error| DomainError::Storage(format!("解密云存储账号 {key} 失败：{error}")))?;
    let account: ObjectStorageAccount = serde_json::from_str(&json)
        .map_err(|error| DomainError::Storage(format!("反序列化云存储账号 {key} 失败：{error}")))?;
    account
        .validate()
        .map_err(|error| DomainError::Storage(format!("解密后的云存储账号 {key} 无效：{error}")))?;
    if account.id.to_string() != key {
        return Err(DomainError::Storage(format!(
            "云存储账号键与内容 ID 不一致：{key}"
        )));
    }
    Ok(account)
}

pub(crate) fn list(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
) -> Result<Vec<ObjectStorageAccount>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(OBJECT_STORAGE_ACCOUNTS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开云存储账号表失败：{error}")))?;
    let cipher = cipher.read();
    let mut accounts = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in table
        .iter()
        .map_err(|error| DomainError::Storage(format!("遍历云存储账号失败：{error}")))?
    {
        let (key, value) =
            entry.map_err(|error| DomainError::Storage(format!("读取云存储账号失败：{error}")))?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            accounts.len(),
            retained_bytes,
            value.value().len(),
            MAX_OBJECT_STORAGE_ACCOUNTS,
            MAX_ACCOUNT_LIST_BYTES,
            "云存储账号列表",
        )?;
        retained_bytes = next_bytes;
        accounts.push(decode(key.value(), value.value(), &cipher)?);
    }
    accounts.sort_by(|left, right| left.name.cmp(&right.name));
    debug!(
        operation = "object_storage_account_list",
        count = accounts.len(),
        "object storage account listing completed"
    );
    Ok(accounts)
}

pub(crate) fn get(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    id: String,
) -> Result<Option<ObjectStorageAccount>> {
    let read_txn = db
        .begin_read()
        .map_err(|error| DomainError::Storage(format!("启动读事务失败：{error}")))?;
    let table = read_txn
        .open_table(OBJECT_STORAGE_ACCOUNTS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开云存储账号表失败：{error}")))?;
    let value = table
        .get(id.as_str())
        .map_err(|error| DomainError::Storage(format!("读取云存储账号 {id} 失败：{error}")))?;
    value
        .map(|value| decode(&id, value.value(), &cipher.read()))
        .transpose()
}

pub(crate) fn save(
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    account: ObjectStorageAccount,
) -> Result<()> {
    let value = encode(&account, &cipher.read())?;
    let id = account.id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut table = write_txn
            .open_table(OBJECT_STORAGE_ACCOUNTS_TABLE)
            .map_err(|error| DomainError::Storage(format!("打开云存储账号表失败：{error}")))?;
        let mut count = 0usize;
        let mut total_bytes = 0usize;
        let mut replaced_bytes = 0usize;
        for entry in table
            .iter()
            .map_err(|error| DomainError::Storage(format!("遍历云存储账号失败：{error}")))?
        {
            let (key, existing) = entry
                .map_err(|error| DomainError::Storage(format!("读取云存储账号失败：{error}")))?;
            (count, total_bytes) = bounded_json::next_collection_budget(
                count,
                total_bytes,
                existing.value().len(),
                MAX_OBJECT_STORAGE_ACCOUNTS,
                MAX_ACCOUNT_LIST_BYTES,
                "云存储账号列表",
            )?;
            if key.value() == id {
                replaced_bytes = existing.value().len();
            }
        }
        let final_count = count.saturating_add(usize::from(replaced_bytes == 0));
        let final_bytes = total_bytes
            .checked_sub(replaced_bytes)
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| DomainError::Storage("云存储账号列表总大小溢出".into()))?;
        bounded_json::ensure_collection_budget(
            final_count,
            final_bytes,
            MAX_OBJECT_STORAGE_ACCOUNTS,
            MAX_ACCOUNT_LIST_BYTES,
            "云存储账号列表",
        )?;
        table
            .insert(id.as_str(), value.as_str())
            .map_err(|error| DomainError::Storage(format!("写入云存储账号 {id} 失败：{error}")))?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "object_storage_account_save", account_id = %account.id, "object storage account saved");
    Ok(())
}

pub(crate) fn delete_with_preference(
    db: Arc<Database>,
    id: ObjectStorageAccountId,
    preference_key: String,
) -> Result<()> {
    let id = id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|error| DomainError::Storage(format!("启动写事务失败：{error}")))?;
    {
        let mut accounts = write_txn
            .open_table(OBJECT_STORAGE_ACCOUNTS_TABLE)
            .map_err(|error| DomainError::Storage(format!("打开云存储账号表失败：{error}")))?;
        accounts
            .remove(id.as_str())
            .map_err(|error| DomainError::Storage(format!("删除云存储账号 {id} 失败：{error}")))?;
    }
    {
        let mut preferences = write_txn
            .open_table(prefs_repo::PREFERENCES_TABLE)
            .map_err(|error| DomainError::Storage(format!("打开 preferences 表失败：{error}")))?;
        preferences
            .remove(preference_key.as_str())
            .map_err(|error| DomainError::Storage(format!("删除云存储工作区偏好失败：{error}")))?;
    }
    write_txn
        .commit()
        .map_err(|error| DomainError::Storage(format!("提交事务失败：{error}")))?;
    info!(operation = "object_storage_account_delete", account_id = %id, "object storage account deleted");
    Ok(())
}

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(OBJECT_STORAGE_ACCOUNTS_TABLE)
        .map_err(|error| DomainError::Storage(format!("打开云存储账号表失败：{error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{CloudProvider, ManualBucket, SecretString};

    #[test]
    fn encrypted_record_does_not_contain_account_fields() {
        let cipher = Cipher::new(&[9; 32]);
        let mut account = ObjectStorageAccount::new("production", CloudProvider::TencentCos);
        account.access_key_id = SecretString::new("secret-id");
        account.access_key_secret = SecretString::new("secret-key");
        account
            .manual_buckets
            .push(ManualBucket::new("storage-test-bucket", "ap-shanghai"));
        let encoded = encode(&account, &cipher).unwrap();

        assert!(!encoded.contains("production"));
        assert!(!encoded.contains("secret-id"));
        assert!(!encoded.contains("secret-key"));
        assert_eq!(
            decode(&account.id.to_string(), &encoded, &cipher).unwrap(),
            account
        );
    }
}
