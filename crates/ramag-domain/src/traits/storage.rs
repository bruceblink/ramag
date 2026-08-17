use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use crate::entities::{
    ClipId, ClipItem, ClipSearchResult, ConnectionConfig, ConnectionId, ObjectStorageAccount,
    ObjectStorageAccountId, QueryHistoryPage, QueryRecord, QueryRecordId, RepoConfig, RepoId,
    SshProfile, SshProfileId,
};
use crate::error::Result;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>>;
    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>>;
    async fn save_connection(&self, config: &ConnectionConfig) -> Result<()>;
    /// 原子新增或更新一批连接；任一条失败时不得留下部分写入。
    async fn save_connections(&self, _configs: &[ConnectionConfig]) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "save_connections".into(),
        ))
    }
    async fn delete_connection(&self, id: &ConnectionId) -> Result<()>;

    async fn list_ssh_profiles(&self) -> Result<Vec<SshProfile>> {
        Err(crate::error::DomainError::NotImplemented(
            "list_ssh_profiles".into(),
        ))
    }

    async fn get_ssh_profile(&self, _id: &SshProfileId) -> Result<Option<SshProfile>> {
        Err(crate::error::DomainError::NotImplemented(
            "get_ssh_profile".into(),
        ))
    }

    async fn save_ssh_profile(&self, _profile: &SshProfile) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "save_ssh_profile".into(),
        ))
    }

    async fn delete_ssh_profile(&self, _id: &SshProfileId) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "delete_ssh_profile".into(),
        ))
    }

    async fn list_object_storage_accounts(&self) -> Result<Vec<ObjectStorageAccount>> {
        Err(crate::error::DomainError::NotImplemented(
            "list_object_storage_accounts".into(),
        ))
    }

    async fn get_object_storage_account(
        &self,
        _id: &ObjectStorageAccountId,
    ) -> Result<Option<ObjectStorageAccount>> {
        Err(crate::error::DomainError::NotImplemented(
            "get_object_storage_account".into(),
        ))
    }

    async fn save_object_storage_account(&self, _account: &ObjectStorageAccount) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "save_object_storage_account".into(),
        ))
    }

    /// 原子删除账号及该账号的对象存储工作区偏好。
    async fn delete_object_storage_account(
        &self,
        _id: &ObjectStorageAccountId,
        _workspace_preference_key: &str,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "delete_object_storage_account".into(),
        ))
    }

    async fn list_repos(&self) -> Result<Vec<RepoConfig>> {
        Err(crate::error::DomainError::NotImplemented(
            "list_repos".into(),
        ))
    }

    async fn save_repo(&self, _config: &RepoConfig) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "save_repo".into(),
        ))
    }

    async fn delete_repo(&self, _id: &RepoId) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "delete_repo".into(),
        ))
    }

    async fn append_history(&self, record: &QueryRecord) -> Result<()>;

    async fn list_history(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<Vec<QueryRecord>>;

    async fn list_history_bounded(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
        max_inline_bytes: u64,
    ) -> Result<QueryHistoryPage> {
        let mut records = self
            .list_history(connection_id, limit.saturating_add(1))
            .await?;
        let mut truncated = records.len() > limit;
        records.truncate(limit);
        let original_len = records.len();
        records = recent_history_within_budget(records, max_inline_bytes);
        truncated |= records.len() < original_len;
        Ok(QueryHistoryPage { records, truncated })
    }

    async fn delete_history(&self, id: &QueryRecordId) -> Result<()>;

    async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()>;

    async fn get_preference(&self, key: &str) -> Result<Option<String>>;
    async fn set_preference(&self, key: &str, value: &str) -> Result<()>;
    async fn delete_preference(&self, key: &str) -> Result<()> {
        self.set_preference(key, "").await
    }

    /// 用主密钥 AES-GCM 加密任意字节（剪贴图片落盘前调，密文存磁盘）
    async fn seal(&self, _plain: &[u8]) -> Result<Vec<u8>> {
        Err(crate::error::DomainError::NotImplemented("seal".into()))
    }

    async fn unseal(&self, _cipher: &[u8]) -> Result<Vec<u8>> {
        Err(crate::error::DomainError::NotImplemented("unseal".into()))
    }

    async fn clip_save(&self, _item: &ClipItem) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_save".into(),
        ))
    }

    async fn clip_get(&self, id: &ClipId) -> Result<Option<ClipItem>> {
        Ok(self
            .clip_list()
            .await?
            .into_iter()
            .find(|item| &item.id == id))
    }

    async fn clip_list(&self) -> Result<Vec<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_list".into(),
        ))
    }

    async fn clip_media_paths(&self) -> Result<Vec<String>> {
        Ok(self
            .clip_list()
            .await?
            .into_iter()
            .flat_map(|item| [item.image_path, item.thumb_path])
            .flatten()
            .collect())
    }

    async fn clip_list_recent(&self, _limit: usize) -> Result<Vec<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_list_recent".into(),
        ))
    }

    /// 取连续的最近记录，并限制直接常驻内存的正文总量。若最新一条本身超限，仍保留该条。
    async fn clip_list_recent_bounded(
        &self,
        limit: usize,
        max_inline_bytes: u64,
    ) -> Result<Vec<ClipItem>> {
        let items = self.clip_list_recent(limit).await?;
        Ok(recent_items_within_budget(items, max_inline_bytes))
    }

    async fn clip_search(&self, _query: &str, _limit: usize) -> Result<Vec<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_search".into(),
        ))
    }

    /// 可取消的全量搜索。默认实现保持旧存储实现兼容；支持取消的实现应在扫描中定期检查。
    async fn clip_search_cancellable(
        &self,
        query: &str,
        limit: usize,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Vec<ClipItem>> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        self.clip_search(query, limit).await
    }

    /// 可取消且限制返回正文总量的搜索。默认实现兼容旧存储；生产实现应在构造结果时限流。
    async fn clip_search_cancellable_bounded(
        &self,
        query: &str,
        limit: usize,
        max_inline_bytes: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ClipSearchResult> {
        let mut items = self
            .clip_search_cancellable(query, limit.saturating_add(1), cancelled)
            .await?;
        let mut truncated = items.len() > limit;
        items.truncate(limit);
        let original_len = items.len();
        items = recent_items_within_budget(items, max_inline_bytes);
        truncated |= items.len() < original_len;
        Ok(ClipSearchResult { items, truncated })
    }

    async fn clip_delete(&self, _id: &ClipId) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_delete".into(),
        ))
    }

    async fn clip_find_by_hash(&self, _hash: &str) -> Result<Option<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_find_by_hash".into(),
        ))
    }

    async fn clip_clear(&self) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_clear".into(),
        ))
    }

    /// 超量 / 过期清理。返回被删条目的 image_path
    async fn clip_prune(&self, _max_items: u32, _max_age_days: u32) -> Result<Vec<String>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_prune".into(),
        ))
    }
}

fn recent_items_within_budget(items: Vec<ClipItem>, max_inline_bytes: u64) -> Vec<ClipItem> {
    if max_inline_bytes == 0 {
        return Vec::new();
    }
    let mut total = 0u64;
    let mut kept = Vec::with_capacity(items.len());
    for item in items {
        let next = total.saturating_add(item.inline_payload_bytes());
        if !kept.is_empty() && next > max_inline_bytes {
            break;
        }
        total = next;
        kept.push(item);
        if total >= max_inline_bytes {
            break;
        }
    }
    kept
}

fn recent_history_within_budget(
    records: Vec<QueryRecord>,
    max_inline_bytes: u64,
) -> Vec<QueryRecord> {
    if max_inline_bytes == 0 {
        return Vec::new();
    }
    let mut total = 0u64;
    let mut kept = Vec::with_capacity(records.len());
    for record in records {
        let next = total.saturating_add(record.inline_payload_bytes());
        if !kept.is_empty() && next > max_inline_bytes {
            break;
        }
        total = next;
        kept.push(record);
        if total >= max_inline_bytes {
            break;
        }
    }
    kept
}
