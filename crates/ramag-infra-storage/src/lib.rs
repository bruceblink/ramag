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
use tracing::info;

use ramag_domain::entities::{
    ClipId, ClipItem, ClipSearchResult, ConnectionConfig, ConnectionId, MAX_CLIPBOARD_SEARCH_BYTES,
    QueryHistoryPage, QueryRecord, QueryRecordId, RepoConfig, RepoId,
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
        // 首次打开建表
        let write_txn = db
            .begin_write()
            .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
        repos::connection_repo::ensure_table(&write_txn)?;
        repos::repo_repo::ensure_table(&write_txn)?;
        repos::history_repo::ensure_table(&write_txn)?;
        repos::clip_repo::ensure_table(&write_txn)?;
        write_txn
            .commit()
            .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;

        let db = Arc::new(db);
        let cipher = Arc::new(RwLock::new(Cipher::new(master_key)));

        // 首启迁移：为存量历史构建时间 / 去重索引（空库或已建则瞬时返回）
        repos::clip_repo::migrate_indexes(db.clone(), cipher.clone())?;
        let _ = repos::connection_repo::list(db.clone(), cipher.clone())?;
        repos::clip_repo::validate_key(db.clone(), cipher.clone())?;

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
    }
    let database = Database::create(path)
        .map_err(|e| DomainError::Storage(format!("打开 redb 数据库失败：{e}")))?;
    set_private_file_permissions(path)
        .map_err(|e| DomainError::Storage(format!("收紧 redb 数据库权限失败：{e}")))?;
    Ok(database)
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
        let key = key.to_string();
        run_blocking(move || repos::prefs_repo::get(db, key)).await
    }

    async fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        let db = self.db.clone();
        let key = key.to_string();
        let value = value.to_string();
        run_blocking(move || repos::prefs_repo::set(db, key, value)).await
    }

    async fn delete_preference(&self, key: &str) -> Result<()> {
        let db = self.db.clone();
        let key = key.to_string();
        run_blocking(move || repos::prefs_repo::delete(db, key)).await
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
mod tests {
    use super::*;
    use ramag_domain::entities::DriverKind;
    use tempfile::TempDir;

    /// 临时目录 + 固定密钥，不污染真实系统凭据库
    fn make_test_storage() -> (RedbStorage, TempDir) {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let path = tmp.path().join("test.redb");
        let key = [0x42u8; 32];
        let storage = RedbStorage::open_with_key(&path, &key).expect("打开测试 storage 失败");
        (storage, tmp)
    }

    #[cfg(unix)]
    #[test]
    fn database_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let (storage, _tmp) = make_test_storage();
        let mode = std::fs::metadata(storage.path())
            .expect("读取测试数据库权限失败")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn only_encrypted_records_require_existing_master_key() {
        let (storage, _tmp) = make_test_storage();
        assert!(!database_has_encrypted_records(&storage.db).unwrap());
        storage.set_preference("theme", "dark").await.unwrap();
        assert!(!database_has_encrypted_records(&storage.db).unwrap());
        storage
            .save_connection(&sample_config("encrypted"))
            .await
            .unwrap();
        assert!(database_has_encrypted_records(&storage.db).unwrap());
        let path = storage.path().to_path_buf();
        drop(storage);
        assert!(RedbStorage::open_with_key(&path, &[0x24; 32]).is_err());
    }

    #[tokio::test]
    async fn preference_delete_removes_value() {
        let (storage, _tmp) = make_test_storage();
        assert!(storage.set_preference("draft", "text").await.is_ok());
        assert!(matches!(
            storage.get_preference("draft").await,
            Ok(Some(value)) if value == "text"
        ));
        assert!(storage.delete_preference("draft").await.is_ok());
        assert!(matches!(storage.get_preference("draft").await, Ok(None)));
    }

    fn sample_config(name: &str) -> ConnectionConfig {
        ConnectionConfig {
            id: ConnectionId::new(),
            name: name.to_string(),
            driver: DriverKind::Mysql,
            host: "127.0.0.1".into(),
            port: 3306,
            username: "root".into(),
            password: "secret-password".into(),
            database: Some("test".into()),
            auth_source: None,
            remark: None,
            environment: None,
            production: false,
            tls: false,
            tls_verify: Default::default(),
            ca_cert_path: None,
            ssh_target: None,
            ssh_port: None,
        }
    }

    #[tokio::test]
    async fn save_and_list() {
        let (storage, _tmp) = make_test_storage();
        let cfg = sample_config("dev");

        storage.save_connection(&cfg).await.unwrap();
        let list = storage.list_connections().await.unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "dev");
        assert_eq!(list[0].password, "secret-password");
    }

    #[tokio::test]
    async fn save_and_get_by_id() {
        let (storage, _tmp) = make_test_storage();
        let cfg = sample_config("prod");

        storage.save_connection(&cfg).await.unwrap();
        let got = storage.get_connection(&cfg.id).await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().name, "prod");
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let (storage, _tmp) = make_test_storage();
        let id = ConnectionId::new();
        let got = storage.get_connection(&id).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn delete_works() {
        let (storage, _tmp) = make_test_storage();
        let cfg = sample_config("a");
        storage.save_connection(&cfg).await.unwrap();
        assert_eq!(storage.list_connections().await.unwrap().len(), 1);

        storage.delete_connection(&cfg.id).await.unwrap();
        assert_eq!(storage.list_connections().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_sorted_by_name() {
        let (storage, _tmp) = make_test_storage();
        for n in &["zebra", "apple", "mongo"] {
            storage.save_connection(&sample_config(n)).await.unwrap();
        }
        let list = storage.list_connections().await.unwrap();
        let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["apple", "mongo", "zebra"]);
    }

    #[tokio::test]
    async fn update_existing() {
        let (storage, _tmp) = make_test_storage();
        let mut cfg = sample_config("dev");
        storage.save_connection(&cfg).await.unwrap();

        cfg.host = "10.0.0.1".to_string();
        storage.save_connection(&cfg).await.unwrap();

        let got = storage.get_connection(&cfg.id).await.unwrap().unwrap();
        assert_eq!(got.host, "10.0.0.1");
    }

    #[tokio::test]
    async fn batch_save_inserts_and_updates_atomically() {
        let (storage, _tmp) = make_test_storage();
        let mut existing = sample_config("existing");
        storage.save_connection(&existing).await.unwrap();

        existing.host = "10.0.0.8".into();
        let added = sample_config("added");
        storage
            .save_connections(&[existing.clone(), added.clone()])
            .await
            .unwrap();

        assert_eq!(
            storage
                .get_connection(&existing.id)
                .await
                .unwrap()
                .unwrap()
                .host,
            "10.0.0.8"
        );
        assert!(storage.get_connection(&added.id).await.unwrap().is_some());
        assert_eq!(storage.list_connections().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn invalid_batch_does_not_leave_partial_connection_updates() {
        let (storage, _tmp) = make_test_storage();
        let original = sample_config("original");
        storage.save_connection(&original).await.unwrap();

        let mut update = original.clone();
        update.host = "10.0.0.9".into();
        let mut invalid = sample_config("invalid");
        invalid.port = 0;

        assert!(
            storage
                .save_connections(&[update, invalid.clone()])
                .await
                .is_err()
        );
        assert_eq!(
            storage
                .get_connection(&original.id)
                .await
                .unwrap()
                .unwrap()
                .host,
            "127.0.0.1"
        );
        assert!(storage.get_connection(&invalid.id).await.unwrap().is_none());
        assert_eq!(storage.list_connections().await.unwrap().len(), 1);
    }

    fn sample_history(connection_id: ConnectionId, sql: &str) -> QueryRecord {
        QueryRecord::new_success(connection_id, "test", sql, 5, 1)
    }

    fn history_stats(storage: &RedbStorage) -> (u64, u64) {
        let txn = storage.db.begin_read().unwrap();
        let table = txn
            .open_table(repos::history_repo::HISTORY_META_TABLE)
            .unwrap();
        let count = table
            .get("record_count")
            .unwrap()
            .map_or(0, |value| value.value());
        let bytes = table
            .get("value_bytes")
            .unwrap()
            .map_or(0, |value| value.value());
        (count, bytes)
    }

    #[tokio::test]
    async fn history_crud_and_connection_filter() {
        let (storage, _tmp) = make_test_storage();
        let first_connection = ConnectionId::new();
        let second_connection = ConnectionId::new();
        let mut first = sample_history(first_connection.clone(), "SELECT 1");
        first.executed_at = Utc::now() - Duration::seconds(1);
        let mut second = sample_history(second_connection.clone(), "SELECT 2");
        second.executed_at = Utc::now();

        storage.append_history(&first).await.unwrap();
        storage.append_history(&second).await.unwrap();
        let (count, bytes) = history_stats(&storage);
        assert_eq!(count, 2);
        assert!(bytes > 0);
        assert_eq!(storage.list_history(None, 10).await.unwrap().len(), 2);
        let newest = storage.list_history(None, 1).await.unwrap();
        assert_eq!(newest[0].id, second.id);
        assert!(storage.list_history(None, 0).await.unwrap().is_empty());

        let filtered = storage
            .list_history(Some(&first_connection), 10)
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, first.id);

        let bounded = storage.list_history_bounded(None, 10, 8).await.unwrap();
        assert_eq!(bounded.records.len(), 1);
        assert!(bounded.truncated);

        storage.delete_history(&first.id).await.unwrap();
        assert_eq!(history_stats(&storage).0, 1);
        assert_eq!(storage.list_history(None, 10).await.unwrap().len(), 1);
        storage
            .clear_history(Some(&second_connection))
            .await
            .unwrap();
        assert_eq!(history_stats(&storage), (0, 0));
        assert!(storage.list_history(None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupted_history_is_reported_instead_of_silently_skipped() {
        let (storage, _tmp) = make_test_storage();
        {
            let txn = storage.db.begin_write().unwrap();
            {
                let mut table = txn.open_table(repos::history_repo::HISTORY_TABLE).unwrap();
                table.insert("corrupt-key", "{not-json").unwrap();
            }
            txn.commit().unwrap();
        }

        let list_error = storage.list_history(None, 10).await.unwrap_err();
        assert!(list_error.to_string().contains("corrupt-key"));
        assert!(
            storage
                .clear_history(Some(&ConnectionId::new()))
                .await
                .is_err()
        );

        // 全量清空无需解析内容，应能作为损坏数据的恢复路径。
        storage.clear_history(None).await.unwrap();
        assert!(storage.list_history(None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupted_repo_record_blocks_unsafe_deduplication() {
        let (storage, _tmp) = make_test_storage();
        {
            let txn = storage.db.begin_write().unwrap();
            {
                let mut table = txn.open_table(repos::repo_repo::REPOS_TABLE).unwrap();
                table.insert("corrupt-repo", "{not-json").unwrap();
            }
            txn.commit().unwrap();
        }

        let repo = RepoConfig::from_path("/tmp/example-repo");
        let error = storage.save_repo(&repo).await.unwrap_err();
        assert!(error.to_string().contains("corrupt-repo"));
        assert!(storage.list_repos().await.is_err());
    }

    use chrono::{Duration, Utc};
    use ramag_domain::entities::{ClipId, ClipKind};

    fn sample_clip(text: &str, age_days: i64) -> ramag_domain::entities::ClipItem {
        let at = Utc::now() - Duration::days(age_days);
        ramag_domain::entities::ClipItem {
            id: ClipId::new(),
            kind: ClipKind::Text,
            text: Some(text.to_string()),
            rtf: None,
            image_path: None,
            thumb_path: None,
            image_dims: None,
            files: Vec::new(),
            preview: text.to_string(),
            source: None,
            byte_size: text.len() as u64,
            content_hash: format!(
                "{:016x}",
                ramag_domain::entities::fnv1a_hash(text.as_bytes())
            ),
            created_at: at,
            last_used_at: at,
        }
    }

    #[tokio::test]
    async fn clip_save_list_roundtrip_sorted() {
        let (storage, _tmp) = make_test_storage();
        storage.clip_save(&sample_clip("old", 3)).await.unwrap();
        storage.clip_save(&sample_clip("new", 0)).await.unwrap();

        let list = storage.clip_list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text.as_deref(), Some("new"));
        assert_eq!(list[1].text.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn clip_find_by_hash_and_delete() {
        let (storage, _tmp) = make_test_storage();
        let clip = sample_clip("dup-me", 0);
        storage.clip_save(&clip).await.unwrap();

        assert_eq!(
            storage
                .clip_get(&clip.id)
                .await
                .unwrap()
                .map(|item| item.id),
            Some(clip.id.clone())
        );
        let found = storage.clip_find_by_hash(&clip.content_hash).await.unwrap();
        assert_eq!(found.unwrap().id, clip.id);
        assert!(storage.clip_find_by_hash("ffff").await.unwrap().is_none());

        storage.clip_delete(&clip.id).await.unwrap();
        assert!(storage.clip_get(&clip.id).await.unwrap().is_none());
        assert!(storage.clip_list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clip_clear_removes_all() {
        let (storage, _tmp) = make_test_storage();
        storage.clip_save(&sample_clip("a", 0)).await.unwrap();
        storage.clip_save(&sample_clip("b", 0)).await.unwrap();

        storage.clip_clear().await.unwrap();
        assert!(storage.clip_list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clip_clear_recovers_from_corrupted_record() {
        let (storage, _tmp) = make_test_storage();
        let clip = sample_clip("corrupted", 0);
        storage.clip_save(&clip).await.unwrap();
        {
            let txn = storage.db.begin_write().unwrap();
            {
                let mut clips = txn.open_table(repos::clip_repo::CLIPS_TABLE).unwrap();
                clips
                    .insert(clip.id.to_string().as_str(), "not-valid-ciphertext")
                    .unwrap();
            }
            txn.commit().unwrap();
        }

        assert!(storage.clip_clear().await.is_ok());
        assert!(storage.clip_list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clip_media_paths_returns_only_referenced_files() {
        let (storage, _tmp) = make_test_storage();
        storage.clip_save(&sample_clip("text", 0)).await.unwrap();
        let mut image = sample_clip("image", 0);
        image.kind = ClipKind::Image;
        image.text = None;
        image.image_path = Some("full.img".into());
        image.thumb_path = Some("thumb.img".into());
        storage.clip_save(&image).await.unwrap();

        let mut paths = storage.clip_media_paths().await.unwrap();
        paths.sort();
        assert_eq!(paths, vec!["full.img", "thumb.img"]);
    }

    #[tokio::test]
    async fn clip_prune_by_count_and_age() {
        let (storage, _tmp) = make_test_storage();
        storage
            .clip_save(&sample_clip("expired", 40))
            .await
            .unwrap();
        storage.clip_save(&sample_clip("kept-1", 1)).await.unwrap();
        storage.clip_save(&sample_clip("kept-2", 0)).await.unwrap();

        // 数量上限 5、保留 30 天：仅超龄 expired 被剔
        storage.clip_prune(5, 30).await.unwrap();
        let rest = storage.clip_list().await.unwrap();
        let texts: Vec<_> = rest.iter().map(|c| c.text.clone().unwrap()).collect();
        assert_eq!(rest.len(), 2);
        assert!(texts.contains(&"kept-1".to_string()));
        assert!(texts.contains(&"kept-2".to_string()));

        // 数量上限 1：只留最新
        storage.clip_prune(1, 30).await.unwrap();
        let rest = storage.clip_list().await.unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].text.as_deref(), Some("kept-2"));
    }

    #[tokio::test]
    async fn clip_list_recent_order_and_limit() {
        let (storage, _tmp) = make_test_storage();
        storage.clip_save(&sample_clip("oldest", 3)).await.unwrap();
        storage.clip_save(&sample_clip("mid", 2)).await.unwrap();
        storage.clip_save(&sample_clip("newest", 0)).await.unwrap();

        // limit 截断 + 最近优先
        let recent = storage.clip_list_recent(2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text.as_deref(), Some("newest"));
        assert_eq!(recent[1].text.as_deref(), Some("mid"));

        // limit 超总数 → 全部返回
        let all = storage.clip_list_recent(100).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn clip_list_recent_respects_inline_byte_budget() {
        let (storage, _tmp) = make_test_storage();
        storage.clip_save(&sample_clip("old", 3)).await.unwrap();
        storage.clip_save(&sample_clip("mid", 2)).await.unwrap();
        storage.clip_save(&sample_clip("newest", 0)).await.unwrap();

        let one = storage.clip_list_recent_bounded(10, 5).await.unwrap();
        assert_eq!(one.len(), 1, "最新一条自身超限时仍应可见");
        assert_eq!(one[0].text.as_deref(), Some("newest"));

        let two = storage.clip_list_recent_bounded(10, 9).await.unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].text.as_deref(), Some("mid"));
    }

    #[tokio::test]
    async fn clip_update_refreshes_recency_without_dup() {
        let (storage, _tmp) = make_test_storage();
        let mut a = sample_clip("a", 5);
        let b = sample_clip("b", 0);
        storage.clip_save(&a).await.unwrap();
        storage.clip_save(&b).await.unwrap();
        assert_eq!(
            storage.clip_list_recent(10).await.unwrap()[0]
                .text
                .as_deref(),
            Some("b")
        );

        // 提升 a（同 id 更新 last_used）→ 旧时间索引项须清除，不得产生重复
        a.last_used_at = Utc::now();
        storage.clip_save(&a).await.unwrap();
        let r = storage.clip_list_recent(10).await.unwrap();
        assert_eq!(r.len(), 2, "更新不应产生重复条目");
        assert_eq!(r[0].text.as_deref(), Some("a"));
        assert_eq!(r[1].text.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn clip_update_removes_stale_hash_mapping() {
        let (storage, _tmp) = make_test_storage();
        let mut clip = sample_clip("hash-change", 0);
        let old_hash = clip.content_hash.clone();
        storage.clip_save(&clip).await.unwrap();

        clip.content_hash = "replacement-hash".into();
        storage.clip_save(&clip).await.unwrap();

        assert!(
            storage
                .clip_find_by_hash(&old_hash)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            storage
                .clip_find_by_hash(&clip.content_hash)
                .await
                .unwrap()
                .unwrap()
                .id,
            clip.id
        );
    }

    #[tokio::test]
    async fn deleting_hash_collision_does_not_remove_other_mapping() {
        let (storage, _tmp) = make_test_storage();
        let first = sample_clip("first", 1);
        let mut second = sample_clip("second", 0);
        second.content_hash = first.content_hash.clone();
        storage.clip_save(&first).await.unwrap();
        storage.clip_save(&second).await.unwrap();

        storage.clip_delete(&first.id).await.unwrap();

        assert_eq!(
            storage
                .clip_find_by_hash(&first.content_hash)
                .await
                .unwrap()
                .unwrap()
                .id,
            second.id
        );
    }

    #[tokio::test]
    async fn dangling_clip_time_index_is_reported() {
        let (storage, _tmp) = make_test_storage();
        let clip = sample_clip("dangling", 0);
        storage.clip_save(&clip).await.unwrap();
        {
            let txn = storage.db.begin_write().unwrap();
            {
                let mut clips = txn.open_table(repos::clip_repo::CLIPS_TABLE).unwrap();
                clips.remove(clip.id.to_string().as_str()).unwrap();
            }
            txn.commit().unwrap();
        }

        let error = storage.clip_list_recent(10).await.unwrap_err();
        assert!(error.to_string().contains("指向缺失条目"));
    }

    #[tokio::test]
    async fn corrupted_clip_meta_blocks_unsafe_update() {
        let (storage, _tmp) = make_test_storage();
        let mut clip = sample_clip("corrupt-meta", 1);
        storage.clip_save(&clip).await.unwrap();
        {
            let txn = storage.db.begin_write().unwrap();
            {
                let mut meta = txn.open_table(repos::clip_repo::CLIP_UUID_META).unwrap();
                meta.insert(clip.id.to_string().as_str(), "invalid-meta")
                    .unwrap();
            }
            txn.commit().unwrap();
        }

        clip.last_used_at = Utc::now();
        let error = storage.clip_save(&clip).await.unwrap_err();
        assert!(error.to_string().contains("索引元数据"));
    }

    #[tokio::test]
    async fn clip_migrate_rebuilds_indexes_from_main_table() {
        let (storage, _tmp) = make_test_storage();
        let c1 = sample_clip("alpha", 2);
        let c2 = sample_clip("beta", 0);
        storage.clip_save(&c1).await.unwrap();
        storage.clip_save(&c2).await.unwrap();

        // 模拟索引丢失：删时间索引表后重建空表（主表保留），mirror open 时 ensure→migrate 流程
        {
            let txn = storage.db.begin_write().unwrap();
            txn.delete_table(repos::clip_repo::CLIP_BY_TIME).unwrap();
            repos::clip_repo::ensure_table(&txn).unwrap();
            txn.commit().unwrap();
        }
        repos::clip_repo::migrate_indexes(storage.db.clone(), storage.cipher.clone()).unwrap();

        let recent = storage.clip_list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].text.as_deref(), Some("beta"));
        assert!(
            storage
                .clip_find_by_hash(&c1.content_hash)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn clip_search_matches_recent_first_and_limit() {
        let (storage, _tmp) = make_test_storage();
        storage
            .clip_save(&sample_clip("hello world", 2))
            .await
            .unwrap();
        storage.clip_save(&sample_clip("foo bar", 1)).await.unwrap();
        storage
            .clip_save(&sample_clip("hello rust", 0))
            .await
            .unwrap();

        // 匹配 + 最近优先
        let r = storage.clip_search("hello", 10).await.unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text.as_deref(), Some("hello rust"));
        assert_eq!(r[1].text.as_deref(), Some("hello world"));

        // ASCII 大小写不敏感匹配走无分配路径
        assert_eq!(storage.clip_search("HELLO", 10).await.unwrap().len(), 2);

        // limit 早停
        let r = storage.clip_search("hello", 1).await.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text.as_deref(), Some("hello rust"));

        // 空 query / 无匹配 → 空
        assert!(storage.clip_search("", 10).await.unwrap().is_empty());
        assert!(storage.clip_search("zzz", 10).await.unwrap().is_empty());
        assert!(
            storage
                .clip_search(&"x".repeat(MAX_CLIPBOARD_SEARCH_BYTES + 1), 10)
                .await
                .is_err()
        );

        let bounded = storage
            .clip_search_cancellable_bounded("hello", 10, 10, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(bounded.items.len(), 1);
        assert!(bounded.truncated);

        // 已取消的搜索不得继续扫描历史
        let cancelled = Arc::new(AtomicBool::new(true));
        assert!(
            storage
                .clip_search_cancellable("hello", 10, cancelled)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
