//! Storage trait：本地持久化统一抽象。infra 层用 redb 实现

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use crate::entities::{
    ClipId, ClipItem, ClipSearchResult, ConnectionConfig, ConnectionId, QueryHistoryPage,
    QueryRecord, QueryRecordId, RepoConfig, RepoId,
};
use crate::error::Result;

#[async_trait]
pub trait Storage: Send + Sync {
    // 连接配置
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>>;
    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>>;
    /// 新增或更新
    async fn save_connection(&self, config: &ConnectionConfig) -> Result<()>;
    async fn delete_connection(&self, id: &ConnectionId) -> Result<()>;

    // Git 仓库（VCS 最近仓库列表）

    /// 按 name 字母序，列表顺序稳定不随打开顺序漂移
    async fn list_repos(&self) -> Result<Vec<RepoConfig>> {
        Err(crate::error::DomainError::NotImplemented(
            "list_repos".into(),
        ))
    }

    /// 新增或更新。VCS 打开仓库后会先更新 `last_opened_at` 再调
    async fn save_repo(&self, _config: &RepoConfig) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "save_repo".into(),
        ))
    }

    /// 仅从最近列表移除，不影响磁盘文件
    async fn delete_repo(&self, _id: &RepoId) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "delete_repo".into(),
        ))
    }

    // 查询历史

    async fn append_history(&self, record: &QueryRecord) -> Result<()>;

    /// 按 executed_at desc。connection_id=None 全部连接
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

    /// connection_id=None 清空全部
    async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()>;

    // 偏好 KV
    async fn get_preference(&self, key: &str) -> Result<Option<String>>;
    async fn set_preference(&self, key: &str, value: &str) -> Result<()>;
    /// 删除单条偏好；默认以空值覆盖，旧 mock 无需同步实现。
    async fn delete_preference(&self, key: &str) -> Result<()> {
        self.set_preference(key, "").await
    }

    /// 用主密钥 AES-GCM 加密任意字节（剪贴图片落盘前调，密文存磁盘）
    async fn seal(&self, _plain: &[u8]) -> Result<Vec<u8>> {
        Err(crate::error::DomainError::NotImplemented("seal".into()))
    }

    /// 解密 `seal` 产物
    async fn unseal(&self, _cipher: &[u8]) -> Result<Vec<u8>> {
        Err(crate::error::DomainError::NotImplemented("unseal".into()))
    }

    // 剪贴板历史（默认 NotImplemented，与 repos 同策略：旧 mock 实现不强制跟进）

    /// 新增或更新（按 id upsert）
    async fn clip_save(&self, _item: &ClipItem) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_save".into(),
        ))
    }

    /// 按 last_used_at desc 返回全部（全量解密，仅孤儿清理 / 导出等低频场景用）
    async fn clip_list(&self) -> Result<Vec<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_list".into(),
        ))
    }

    /// 返回历史引用的媒体路径。默认实现兼容旧存储；高容量实现应流式提取，避免保留全文。
    async fn clip_media_paths(&self) -> Result<Vec<String>> {
        Ok(self
            .clip_list()
            .await?
            .into_iter()
            .flat_map(|item| [item.image_path, item.thumb_path])
            .flatten()
            .collect())
    }

    /// 取最近 limit 条（最近优先）。窗口缓存预加载用，走时间索引只解密这 limit 条
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

    /// 全量搜索（最近优先，匹配 preview/text，到 limit 停）。覆盖缓存窗口之外的历史
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

    /// 内容指纹查重（连续复制同内容时提升旧条目）
    async fn clip_find_by_hash(&self, _hash: &str) -> Result<Option<ClipItem>> {
        Err(crate::error::DomainError::NotImplemented(
            "clip_find_by_hash".into(),
        ))
    }

    /// 清空全部历史。返回被删条目的 image_path（调用方清理落盘文件）
    async fn clip_clear(&self) -> Result<Vec<String>> {
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
