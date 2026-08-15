//! 各表 repo（同步 + redb 事务）。lib.rs 包 run_blocking 异步化

pub(crate) mod bounded_json;
pub(crate) mod clip_repo;
pub(crate) mod connection_repo;
pub(crate) mod history_repo;
pub(crate) mod object_storage_account_repo;
pub(crate) mod prefs_repo;
pub(crate) mod repo_repo;
pub(crate) mod ssh_profile_repo;

use ramag_domain::error::Result;

/// 启动时在同一事务内补齐全部存储结构；已有表和数据保持不变。
pub(crate) fn ensure_schema(write_txn: &redb::WriteTransaction) -> Result<()> {
    connection_repo::ensure_table(write_txn)?;
    repo_repo::ensure_table(write_txn)?;
    history_repo::ensure_table(write_txn)?;
    object_storage_account_repo::ensure_table(write_txn)?;
    prefs_repo::ensure_table(write_txn)?;
    ssh_profile_repo::ensure_table(write_txn)?;
    clip_repo::ensure_table(write_txn)?;
    Ok(())
}
