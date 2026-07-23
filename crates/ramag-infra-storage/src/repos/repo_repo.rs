//! Git 仓库（VCS 最近列表）CRUD。无敏感字段不加密；按 path 去重避免同物理仓库重复打开堆积

use std::sync::Arc;

use redb::{Database, ReadableDatabase as _, ReadableTable, TableDefinition};
use tracing::{debug, info};

use ramag_domain::entities::{RepoConfig, RepoId};
use ramag_domain::error::{DomainError, Result};

use crate::repos::bounded_json;

const MAX_REPO_RECORD_BYTES: usize = 1024 * 1024;
const MAX_REPO_RECORDS: usize = 2048;
const MAX_REPO_LIST_BYTES: usize = 64 * 1024 * 1024;

/// 键为 RepoId UUID，值为 RepoConfig JSON。
pub(crate) const REPOS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("repos");

fn decode_repo(key: &str, value: &str) -> Result<RepoConfig> {
    bounded_json::ensure_len(
        value.len(),
        MAX_REPO_RECORD_BYTES,
        &format!("仓库记录 {key}"),
    )?;
    serde_json::from_str(value)
        .map_err(|error| DomainError::Storage(format!("反序列化仓库 {key} 失败：{error}")))
}

pub(crate) fn list(db: Arc<Database>) -> Result<Vec<RepoConfig>> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("启动读事务失败：{e}")))?;
    let table = read_txn
        .open_table(REPOS_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 repos 表失败：{e}")))?;

    let mut out: Vec<RepoConfig> = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in table
        .iter()
        .map_err(|e| DomainError::Storage(e.to_string()))?
    {
        let (key, value) = entry.map_err(|e| DomainError::Storage(e.to_string()))?;
        let (_, next_bytes) = bounded_json::next_collection_budget(
            out.len(),
            retained_bytes,
            value.value().len(),
            MAX_REPO_RECORDS,
            MAX_REPO_LIST_BYTES,
            "仓库列表",
        )?;
        retained_bytes = next_bytes;
        out.push(decode_repo(key.value(), value.value())?);
    }
    // 按 name 字母序，顺序稳定不漂移
    out.sort_by(|a, b| a.name.cmp(&b.name));
    debug!(count = out.len(), "repository listing completed");
    Ok(out)
}

pub(crate) fn save(db: Arc<Database>, config: RepoConfig) -> Result<()> {
    let json = bounded_json::serialize(&config, MAX_REPO_RECORD_BYTES, "仓库记录")?;
    let id_str = config.id.to_string();
    let target_path = config.path.clone();

    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(REPOS_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 repos 表失败：{e}")))?;

        // 同 path 去重：driver 每次 open 都生成新 RepoId，事务内先删旧 path 记录再写
        let mut stale_keys: Vec<String> = Vec::new();
        let mut stale_bytes = 0usize;
        let mut item_count = 0usize;
        let mut total_bytes = 0usize;
        let mut replaced_bytes = None;
        for entry in table
            .iter()
            .map_err(|e| DomainError::Storage(e.to_string()))?
        {
            let (k, v) = entry.map_err(|e| DomainError::Storage(e.to_string()))?;
            (item_count, total_bytes) = bounded_json::next_collection_budget(
                item_count,
                total_bytes,
                v.value().len(),
                MAX_REPO_RECORDS,
                MAX_REPO_LIST_BYTES,
                "仓库列表",
            )?;
            if k.value() == id_str {
                replaced_bytes = Some(v.value().len());
            }
            let cfg = decode_repo(k.value(), v.value()).map_err(|error| {
                DomainError::Storage(format!(
                    "读取仓库记录 {} 失败，无法安全去重：{}",
                    k.value(),
                    error.message()
                ))
            })?;
            if cfg.path == target_path && k.value() != id_str {
                stale_bytes = stale_bytes.saturating_add(v.value().len());
                stale_keys.push(k.value().to_string());
            }
        }
        let final_count = item_count
            .saturating_sub(stale_keys.len())
            .saturating_add(usize::from(replaced_bytes.is_none()));
        let final_bytes = total_bytes
            .saturating_sub(stale_bytes)
            .saturating_sub(replaced_bytes.unwrap_or(0))
            .checked_add(json.len())
            .ok_or_else(|| DomainError::Storage("仓库列表总数据大小溢出".into()))?;
        bounded_json::ensure_collection_budget(
            final_count,
            final_bytes,
            MAX_REPO_RECORDS,
            MAX_REPO_LIST_BYTES,
            "仓库列表",
        )?;
        for k in stale_keys {
            table
                .remove(k.as_str())
                .map_err(|e| DomainError::Storage(format!("清理重复记录失败：{e}")))?;
        }

        table
            .insert(id_str.as_str(), json.as_str())
            .map_err(|e| DomainError::Storage(format!("写入仓库失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

    info!(repo_id = %config.id, name = %config.name, "repository saved");
    Ok(())
}

pub(crate) fn delete(db: Arc<Database>, id: RepoId) -> Result<()> {
    let id_str = id.to_string();
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(REPOS_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 repos 表失败：{e}")))?;
        table
            .remove(id_str.as_str())
            .map_err(|e| DomainError::Storage(format!("删除仓库失败：{e}")))?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

    info!(repo_id = %id_str, "repository deleted");
    Ok(())
}

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(REPOS_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 repos 表失败：{e}")))?;
    Ok(())
}
