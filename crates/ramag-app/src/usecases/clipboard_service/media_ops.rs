//! 剪贴板媒体读取、删除撤销与孤儿清理。

use ramag_domain::entities::{ClipId, ClipItem};
use ramag_domain::error::{DomainError, Result};
use tracing::{debug, warn};

use super::{ClipboardService, MAX_AGE_DAYS, MAX_ITEMS};

const MAX_ORPHAN_REMOVALS_PER_RUN: usize = 10_000;

impl ClipboardService {
    /// 读原图明文 PNG（读密文 → 解密）；非图片或无图返回 None。
    pub async fn load_image(&self, item: &ClipItem) -> Result<Option<Vec<u8>>> {
        match &item.image_path {
            Some(path) => {
                let encrypted = self.read_media(path.clone()).await?;
                Ok(Some(self.storage.unseal(&encrypted).await?))
            }
            None => Ok(None),
        }
    }

    /// 读缩略图明文 PNG（列表展示用）；无缩略图回退原图。
    pub async fn load_thumb(&self, item: &ClipItem) -> Result<Option<Vec<u8>>> {
        match &item.thumb_path {
            Some(path) => {
                let encrypted = self.read_media(path.clone()).await?;
                Ok(Some(self.storage.unseal(&encrypted).await?))
            }
            None => self.load_image(item).await,
        }
    }

    /// 清理磁盘上未被任何历史条目引用的媒体文件。
    pub async fn cleanup_orphans(&self) -> Result<usize> {
        // 引用快照与目录删除必须和媒体落盘/入库串行，否则会把“已落盘、尚未入库”的新文件误判为孤儿。
        let _guard = self.history_mutation_lock.lock().await;
        let referenced: std::collections::HashSet<String> =
            self.storage.clip_media_paths().await?.into_iter().collect();
        let driver = self.driver.clone();
        let removed = crate::run_blocking(move || {
            let mut removed = 0;
            for path in driver.list_media()? {
                if !referenced.contains(&path) {
                    if removed >= MAX_ORPHAN_REMOVALS_PER_RUN {
                        warn!(
                            limit = MAX_ORPHAN_REMOVALS_PER_RUN,
                            "orphan media cleanup budget reached"
                        );
                        break;
                    }
                    if let Err(e) = driver.remove_media(&path) {
                        warn!(error = %e, path, "remove orphan media failed");
                    } else {
                        removed += 1;
                    }
                }
            }
            Ok(removed)
        })
        .await?;
        if removed > 0 {
            debug!(removed, "orphan media cleaned");
        }
        Ok(removed)
    }

    pub async fn delete(&self, item: &ClipItem) -> Result<Option<u64>> {
        let _guard = self.history_mutation_lock.lock().await;
        let current = self.current_clip(&item.id).await?;
        self.storage.clip_delete(&current.id).await?;
        self.cache_remove(&current.id);
        let media: Vec<String> = [&current.image_path, &current.thumb_path]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let cleanup_token = (!media.is_empty())
            .then(|| self.pending_media_deletes.stage(current.id.clone(), media));
        self.bump();
        Ok(cleanup_token)
    }

    /// 撤销删除：把条目连同尚在宽限期内的媒体引用原样回存。
    pub async fn restore(&self, item: ClipItem) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        if let Some(existing) = self.storage.clip_get(&item.id).await? {
            self.cache_upsert(existing);
            return Ok(());
        }
        let duplicate = self.storage.clip_find_by_hash(&item.content_hash).await?;
        let pending_media = if item.image_path.is_some() || item.thumb_path.is_some() {
            Some(
                self.pending_media_deletes
                    .take_for_restore(&item.id)
                    .ok_or_else(|| {
                        DomainError::Storage("图片撤销窗口已过期，媒体文件已清理".into())
                    })?,
            )
        } else {
            None
        };
        if let Some(existing) = duplicate
            && existing.id != item.id
            && clip_items_share_payload(&existing, &item)
        {
            self.protect_item_media(&item);
            self.cache_upsert(existing);
            self.bump();
            return Ok(());
        }
        if let Err(e) = self.storage.clip_save(&item).await {
            if let Some((token, paths)) = pending_media {
                self.pending_media_deletes
                    .put_back(item.id.clone(), token, paths);
            }
            return Err(e);
        }
        self.protect_item_media(&item);
        self.cache_upsert(item);
        self.bump();
        Ok(())
    }

    /// 撤销窗口到期后物理清理媒体；已撤销或文本条目为空操作。
    pub async fn finalize_deleted_media(&self, id: &ClipId, token: u64) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        let Some(paths) = self.pending_media_deletes.expire(id, token) else {
            return Ok(());
        };
        let driver = self.driver.clone();
        crate::run_blocking(move || {
            let mut last_error = None;
            for path in paths {
                if let Err(e) = driver.remove_media(&path) {
                    last_error = Some(e);
                }
            }
            // 到期即不可再恢复；清理失败的文件留给下次启动的孤儿扫描重试，不能重挂为可恢复状态，
            // 否则部分文件已删时会恢复出损坏的图片引用。
            match last_error {
                Some(e) => Err(e),
                None => Ok(()),
            }
        })
        .await
    }

    pub async fn clear(&self) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        self.storage.clip_clear().await?;
        self.cache_clear();
        self.pending_media_deletes.clear();
        let driver = self.driver.clone();
        let cleanup_result = crate::run_blocking(move || driver.clear_media()).await;
        self.bump();
        cleanup_result
            .map_err(|e| DomainError::Storage(format!("历史已清空，但媒体文件清理未完成：{e}")))
    }

    pub(super) async fn prune(&self) {
        match self.storage.clip_prune(MAX_ITEMS, MAX_AGE_DAYS).await {
            Ok(images) => {
                if let Err(e) = self.cleanup_media(images).await {
                    warn!(error = %e, "remove pruned clip media failed");
                }
            }
            Err(e) => warn!(error = %e, "clip prune failed"),
        }
    }

    pub(super) async fn cleanup_media(&self, paths: Vec<String>) -> Result<()> {
        let driver = self.driver.clone();
        crate::run_blocking(move || {
            for path in paths {
                if let Err(e) = driver.remove_media(&path) {
                    warn!(error = %e, path, "remove clip media failed");
                }
            }
            Ok(())
        })
        .await
    }

    pub(super) fn unprotected_staged_media(&self, paths: Vec<String>) -> Vec<String> {
        paths
            .into_iter()
            .filter(|path| !self.pending_media_deletes.contains_path(path))
            .collect()
    }

    pub(super) fn protect_item_media(&self, item: &ClipItem) {
        self.pending_media_deletes.protect_paths(
            [&item.image_path, &item.thumb_path]
                .into_iter()
                .flatten()
                .map(String::as_str),
        );
    }

    pub(super) async fn persist_media(&self, key: String, bytes: Vec<u8>) -> Result<String> {
        let driver = self.driver.clone();
        crate::run_blocking(move || driver.persist_media(&key, &bytes)).await
    }

    async fn read_media(&self, path: String) -> Result<Vec<u8>> {
        let driver = self.driver.clone();
        crate::run_blocking(move || driver.read_media(&path)).await
    }
}

pub(super) fn clip_items_share_payload(left: &ClipItem, right: &ClipItem) -> bool {
    if left.kind != right.kind || left.content_hash != right.content_hash {
        return false;
    }
    match left.kind {
        ramag_domain::entities::ClipKind::Image => {
            left.byte_size == right.byte_size
                && left.image_dims == right.image_dims
                && left.image_path == right.image_path
                && left.thumb_path == right.thumb_path
        }
        ramag_domain::entities::ClipKind::Files => left.files == right.files,
        _ => left.text == right.text && left.rtf == right.rtf,
    }
}
