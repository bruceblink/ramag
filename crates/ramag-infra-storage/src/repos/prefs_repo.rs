//! 偏好 KV：主题 / 上次连接 ID / 窗口尺寸等单条 string

use std::sync::Arc;

use redb::{Database, ReadableDatabase as _, TableDefinition};

use ramag_domain::error::{DomainError, Result};

use crate::repos::bounded_json;

const MAX_PREFERENCE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) const PREFERENCES_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("preferences");

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn
        .open_table(PREFERENCES_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 preferences 表失败：{e}")))?;
    Ok(())
}

pub(crate) fn get(db: Arc<Database>, key: String) -> Result<Option<String>> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("启动读事务失败：{e}")))?;
    let table = read_txn
        .open_table(PREFERENCES_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 preferences 表失败：{e}")))?;
    let value = table
        .get(key.as_str())
        .map_err(|e| DomainError::Storage(format!("读偏好失败：{e}")))?;
    let Some(value) = value else {
        return Ok(None);
    };
    bounded_json::ensure_len(
        value.value().len(),
        MAX_PREFERENCE_BYTES,
        &format!("偏好 {key}"),
    )?;
    Ok(Some(value.value().to_string()))
}

pub(crate) fn set(db: Arc<Database>, key: String, value: String) -> Result<()> {
    bounded_json::ensure_len(value.len(), MAX_PREFERENCE_BYTES, &format!("偏好 {key}"))?;
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(PREFERENCES_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 preferences 表失败：{e}")))?;
        table
            .insert(key.as_str(), value.as_str())
            .map_err(|e| DomainError::Storage(format!("写偏好失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    Ok(())
}

pub(crate) fn delete(db: Arc<Database>, key: String) -> Result<()> {
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(PREFERENCES_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 preferences 表失败：{e}")))?;
        table
            .remove(key.as_str())
            .map_err(|e| DomainError::Storage(format!("删除偏好失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    Ok(())
}
