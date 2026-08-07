#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 本地存储：redb 嵌入式 DB；密码 AES-GCM 加密，主密钥存系统凭据库。
//! 业务按表拆到 `repos` 子模块（同步），lib 用 `run_blocking` 异步化。
//! 数据目录由 `directories::ProjectDirs` 按当前平台定位。

pub mod encryption;
pub mod keyring;
mod repos;
mod worker_pool;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_trait::async_trait;
use directories::ProjectDirs;
use parking_lot::RwLock;
use redb::{Database, ReadableDatabase as _, ReadableTableMetadata as _, TableError};
use tracing::{debug, info, warn};

use ramag_domain::entities::{
    ClipId, ClipItem, ClipSearchResult, ConnectionConfig, ConnectionId, MAX_CLIPBOARD_SEARCH_BYTES,
    QueryHistoryPage, QueryRecord, QueryRecordId, RepoConfig, RepoId, SshProfile, SshProfileId,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::Storage;

use crate::encryption::Cipher;
use crate::worker_pool::run as run_blocking;

pub struct RedbStorage {
    db: Arc<Database>,
    cipher: Arc<RwLock<Cipher>>,
    path: PathBuf,
}

impl RedbStorage {
    /// 默认路径，首次会创建文件并在系统凭据库生成主密钥
    pub fn open_default() -> Result<Self> {
        let path = default_db_path()?;
        Self::open(&path)
    }

    /// 生产入口：从系统凭据库读取主密钥
    pub fn open(path: &Path) -> Result<Self> {
        // 先获取 redb 的进程级文件锁，防止两个首次启动进程竞争生成并覆盖主密钥。
        let db = open_database(path)?;
        let allow_create = !database_has_encrypted_records(&db)?;
        let master_key = keyring::get_or_create_master_key(allow_create)?;
        Self::initialize(db, path, &master_key)
    }

    /// 测试入口：注入固定密钥，避免污染真实系统凭据库
    pub fn open_with_key(path: &Path, master_key: &[u8; 32]) -> Result<Self> {
        let db = open_database(path)?;
        Self::initialize(db, path, master_key)
    }

    fn initialize(db: Database, path: &Path, master_key: &[u8; 32]) -> Result<Self> {
        // 每次启动都补齐完整结构，兼容全新数据库和旧版本升级。
        let write_txn = db
            .begin_write()
            .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
        repos::ensure_schema(&write_txn)?;
        write_txn
            .commit()
            .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

        let db = Arc::new(db);
        let cipher = Arc::new(RwLock::new(Cipher::new(master_key)));

        // 首启迁移：为存量历史构建时间 / 去重索引（空库或已建则瞬时返回）
        repos::clip_repo::migrate_indexes(db.clone(), cipher.clone())?;
        let _ = repos::connection_repo::list(db.clone(), cipher.clone())?;
        let _ = repos::ssh_profile_repo::list(db.clone(), cipher.clone())?;
        repos::clip_repo::validate_key(db.clone(), cipher.clone())?;
        repos::clip_repo::initialize_search_index(db.clone(), cipher.clone())?;

        info!(path = %path.display(), "redb storage opened");

        Ok(Self {
            db,
            cipher,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn default_db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "ramag", "ramag")
        .ok_or_else(|| DomainError::Storage("无法定位用户目录".into()))?;
    Ok(dirs.data_dir().join("ramag.redb"))
}

fn open_database(path: &Path) -> Result<Database> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| DomainError::Storage(format!("创建数据目录失败：{e}")))?;
        reject_symlink(parent, "数据目录")?;
    }
    reject_symlink(path, "数据库文件")?;
    let database = Database::create(path)
        .map_err(|e| DomainError::Storage(format!("打开 redb 数据库失败：{e}")))?;
    set_private_file_permissions(path)
        .map_err(|e| DomainError::Storage(format!("收紧 redb 数据库权限失败：{e}")))?;
    Ok(database)
}

fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::Storage(format!(
            "{label}不能是符号链接：{}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage(format!(
            "检查{label}失败 {}：{error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// 连接密码和剪贴历史使用主密钥；只有这两张表存在记录时才禁止重建密钥。
fn database_has_encrypted_records(db: &Database) -> Result<bool> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("检查加密数据失败：{e}")))?;
    for definition in [
        repos::connection_repo::CONNECTIONS_TABLE,
        repos::clip_repo::CLIPS_TABLE,
        repos::ssh_profile_repo::SSH_PROFILES_TABLE,
    ] {
        match read_txn.open_table(definition) {
            Ok(table)
                if !table
                    .is_empty()
                    .map_err(|e| DomainError::Storage(format!("检查加密数据表失败：{e}")))? =>
            {
                return Ok(true);
            }
            Ok(_) | Err(TableError::TableDoesNotExist(_)) => {}
            Err(error) => {
                return Err(DomainError::Storage(format!("打开加密数据表失败：{error}")));
            }
        }
    }
    Ok(false)
}

fn validate_clip_search_query(query: &str) -> Result<()> {
    if query.len() > MAX_CLIPBOARD_SEARCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴历史搜索词超过 {MAX_CLIPBOARD_SEARCH_BYTES} bytes 上限"
        )));
    }
    Ok(())
}

#[async_trait]
impl Storage for RedbStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::connection_repo::list(db, cipher)).await
    }

    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let id_str = id.to_string();
        run_blocking(move || repos::connection_repo::get(db, cipher, id_str)).await
    }

    async fn save_connection(&self, config: &ConnectionConfig) -> Result<()> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let config = config.clone();
        run_blocking(move || repos::connection_repo::save(db, cipher, config)).await
    }

    async fn save_connections(&self, configs: &[ConnectionConfig]) -> Result<()> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let configs = configs.to_vec();
        run_blocking(move || repos::connection_repo::save_many(db, cipher, configs)).await
    }

    async fn delete_connection(&self, id: &ConnectionId) -> Result<()> {
        let db = self.db.clone();
        let id_str = id.to_string();
        run_blocking(move || repos::connection_repo::delete(db, id_str)).await
    }

    async fn list_ssh_profiles(&self) -> Result<Vec<SshProfile>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::ssh_profile_repo::list(db, cipher)).await
    }

    async fn get_ssh_profile(&self, id: &SshProfileId) -> Result<Option<SshProfile>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let id = id.to_string();
        run_blocking(move || repos::ssh_profile_repo::get(db, cipher, id)).await
    }

    async fn save_ssh_profile(&self, profile: &SshProfile) -> Result<()> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let profile = profile.clone();
        run_blocking(move || repos::ssh_profile_repo::save(db, cipher, profile)).await
    }

    async fn delete_ssh_profile(&self, id: &SshProfileId) -> Result<()> {
        let db = self.db.clone();
        let id = id.clone();
        run_blocking(move || repos::ssh_profile_repo::delete(db, id)).await
    }

    async fn list_repos(&self) -> Result<Vec<RepoConfig>> {
        let db = self.db.clone();
        run_blocking(move || repos::repo_repo::list(db)).await
    }

    async fn save_repo(&self, config: &RepoConfig) -> Result<()> {
        let db = self.db.clone();
        let config = config.clone();
        run_blocking(move || repos::repo_repo::save(db, config)).await
    }

    async fn delete_repo(&self, id: &RepoId) -> Result<()> {
        let db = self.db.clone();
        let id = id.clone();
        run_blocking(move || repos::repo_repo::delete(db, id)).await
    }

    async fn append_history(&self, record: &QueryRecord) -> Result<()> {
        let db = self.db.clone();
        let record = record.clone();
        run_blocking(move || repos::history_repo::append(db, record)).await
    }

    async fn list_history(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        let db = self.db.clone();
        let conn_filter = connection_id.cloned();
        run_blocking(move || repos::history_repo::list(db, conn_filter, limit)).await
    }

    async fn list_history_bounded(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
        max_inline_bytes: u64,
    ) -> Result<QueryHistoryPage> {
        let db = self.db.clone();
        let conn_filter = connection_id.cloned();
        run_blocking(move || {
            repos::history_repo::list_bounded(db, conn_filter, limit, max_inline_bytes)
        })
        .await
    }

    async fn delete_history(&self, id: &QueryRecordId) -> Result<()> {
        let db = self.db.clone();
        let id = id.clone();
        run_blocking(move || repos::history_repo::delete(db, id)).await
    }

    async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()> {
        let db = self.db.clone();
        let conn_filter = connection_id.cloned();
        run_blocking(move || repos::history_repo::clear(db, conn_filter)).await
    }

    async fn get_preference(&self, key: &str) -> Result<Option<String>> {
        let db = self.db.clone();
        let key_owned = key.to_string();
        let result = run_blocking(move || repos::prefs_repo::get(db, key_owned)).await;
        if let Err(error) = &result {
            warn!(error = %error, preference = key, "load preference failed");
        }
        result
    }

    async fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        let db = self.db.clone();
        let key_owned = key.to_string();
        let value = value.to_string();
        let result = run_blocking(move || repos::prefs_repo::set(db, key_owned, value)).await;
        match &result {
            Ok(()) => debug!(preference = key, "preference saved"),
            Err(error) => warn!(error = %error, preference = key, "save preference failed"),
        }
        result
    }

    async fn delete_preference(&self, key: &str) -> Result<()> {
        let db = self.db.clone();
        let key_owned = key.to_string();
        let result = run_blocking(move || repos::prefs_repo::delete(db, key_owned)).await;
        match &result {
            Ok(()) => debug!(preference = key, "preference deleted"),
            Err(error) => warn!(error = %error, preference = key, "delete preference failed"),
        }
        result
    }

    async fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher.clone();
        let plain = plain.to_vec();
        run_blocking(move || cipher.read().encrypt_bytes(&plain)).await
    }

    async fn unseal(&self, cipher_blob: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher.clone();
        let blob = cipher_blob.to_vec();
        run_blocking(move || cipher.read().decrypt_bytes(&blob)).await
    }

    async fn clip_save(&self, item: &ClipItem) -> Result<()> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let item = item.clone();
        run_blocking(move || repos::clip_repo::save(db, cipher, item)).await
    }

    async fn clip_get(&self, id: &ClipId) -> Result<Option<ClipItem>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let id = id.to_string();
        run_blocking(move || repos::clip_repo::get(db, cipher, id)).await
    }

    async fn clip_list(&self) -> Result<Vec<ClipItem>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::clip_repo::list(db, cipher)).await
    }

    async fn clip_media_paths(&self) -> Result<Vec<String>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::clip_repo::media_paths(db, cipher)).await
    }

    async fn clip_list_recent(&self, limit: usize) -> Result<Vec<ClipItem>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::clip_repo::list_recent(db, cipher, limit)).await
    }

    async fn clip_list_recent_bounded(
        &self,
        limit: usize,
        max_inline_bytes: u64,
    ) -> Result<Vec<ClipItem>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || {
            repos::clip_repo::list_recent_bounded(db, cipher, limit, max_inline_bytes)
        })
        .await
    }

    async fn clip_search(&self, query: &str, limit: usize) -> Result<Vec<ClipItem>> {
        validate_clip_search_query(query)?;
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let query = query.to_string();
        run_blocking(move || repos::clip_repo::search(db, cipher, query, limit)).await
    }

    async fn clip_search_cancellable(
        &self,
        query: &str,
        limit: usize,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Vec<ClipItem>> {
        validate_clip_search_query(query)?;
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let query = query.to_string();
        run_blocking(move || {
            repos::clip_repo::search_cancellable(db, cipher, query, limit, cancelled)
        })
        .await
    }

    async fn clip_search_cancellable_bounded(
        &self,
        query: &str,
        limit: usize,
        max_inline_bytes: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ClipSearchResult> {
        validate_clip_search_query(query)?;
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let query = query.to_string();
        run_blocking(move || {
            repos::clip_repo::search_cancellable_bounded(
                db,
                cipher,
                query,
                limit,
                max_inline_bytes,
                cancelled,
            )
        })
        .await
    }

    async fn clip_delete(&self, id: &ClipId) -> Result<()> {
        let db = self.db.clone();
        let id_str = id.to_string();
        run_blocking(move || repos::clip_repo::delete(db, id_str)).await
    }

    async fn clip_find_by_hash(&self, hash: &str) -> Result<Option<ClipItem>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        let hash = hash.to_string();
        run_blocking(move || repos::clip_repo::find_by_hash(db, cipher, hash)).await
    }

    async fn clip_clear(&self) -> Result<()> {
        let db = self.db.clone();
        run_blocking(move || repos::clip_repo::clear(db)).await
    }

    async fn clip_prune(&self, max_items: u32, max_age_days: u32) -> Result<Vec<String>> {
        let db = self.db.clone();
        let cipher = self.cipher.clone();
        run_blocking(move || repos::clip_repo::prune(db, cipher, max_items, max_age_days)).await
    }
}

#[cfg(test)]
mod tests;
