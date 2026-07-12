//! 剪贴板媒体读取、删除撤销与孤儿清理。

use ramag_domain::entities::{ClipId, ClipItem};
use ramag_domain::error::{DomainError, Result};
use tracing::{debug, warn};

use super::{ClipboardService, MAX_AGE_DAYS, MAX_ITEMS};

impl ClipboardService {
    /// 读原图明文 PNG（读密文 → 解密）；非图片或无图返回 None。
    pub async fn load_image(&self, item: &ClipItem) -> Result<Option<Vec<u8>>> {
        match &item.image_path {
            Some(path) => {
                let encrypted = self.driver.read_media(path)?;
                Ok(Some(self.storage.unseal(&encrypted).await?))
            }
            None => Ok(None),
        }
    }

    /// 读缩略图明文 PNG（列表展示用）；无缩略图回退原图。
    pub async fn load_thumb(&self, item: &ClipItem) -> Result<Option<Vec<u8>>> {
        match &item.thumb_path {
            Some(path) => {
                let encrypted = self.driver.read_media(path)?;
                Ok(Some(self.storage.unseal(&encrypted).await?))
            }
            None => self.load_image(item).await,
        }
    }

    /// 清理磁盘上未被任何历史条目引用的媒体文件。
    pub async fn cleanup_orphans(&self) -> Result<usize> {
        let items = self.storage.clip_list().await?;
        let mut referenced = std::collections::HashSet::new();
        for item in &items {
            if let Some(path) = &item.image_path {
                referenced.insert(path.clone());
            }
            if let Some(path) = &item.thumb_path {
                referenced.insert(path.clone());
            }
        }
        let mut removed = 0;
        for path in self.driver.list_media()? {
            if !referenced.contains(&path) {
                if let Err(e) = self.driver.remove_media(&path) {
                    warn!(error = %e, path, "remove orphan media failed");
                } else {
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            debug!(removed, "orphan media cleaned");
        }
        Ok(removed)
    }

    pub async fn delete(&self, item: &ClipItem) -> Result<Option<u64>> {
        self.storage.clip_delete(&item.id).await?;
        self.cache_remove(&item.id);
        let media: Vec<String> = [&item.image_path, &item.thumb_path]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let cleanup_token =
            (!media.is_empty()).then(|| self.pending_media_deletes.stage(item.id.clone(), media));
        self.bump();
        Ok(cleanup_token)
    }

    /// 撤销删除：把条目连同尚在宽限期内的媒体引用原样回存。
    pub async fn restore(&self, item: ClipItem) -> Result<()> {
        let media = if item.image_path.is_some() || item.thumb_path.is_some() {
            self.pending_media_deletes
                .take_for_restore(&item.id)
                .ok_or_else(|| DomainError::Storage("图片撤销窗口已过期，媒体文件已清理".into()))?
        } else {
            Vec::new()
        };
        if let Err(e) = self.storage.clip_save(&item).await {
            if !media.is_empty() {
                let _ = self.pending_media_deletes.stage(item.id.clone(), media);
            }
            return Err(e);
        }
        self.cache_upsert(item);
        self.bump();
        Ok(())
    }

    /// 撤销窗口到期后物理清理媒体；已撤销或文本条目为空操作。
    pub fn finalize_deleted_media(&self, id: &ClipId, token: u64) -> Result<()> {
        let Some(paths) = self.pending_media_deletes.expire(id, token) else {
            return Ok(());
        };
        let mut last_error = None;
        for path in paths {
            if let Err(e) = self.driver.remove_media(&path) {
                last_error = Some(e);
            }
        }
        // 到期即不可再恢复；清理失败的文件留给下次启动的孤儿扫描重试，不能重挂为可恢复状态，
        // 否则部分文件已删时会恢复出损坏的图片引用。
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub async fn clear(&self) -> Result<()> {
        let images = self.storage.clip_clear().await?;
        self.cache_clear();
        self.cleanup_media(images);
        self.bump();
        Ok(())
    }

    pub(super) async fn prune(&self) {
        match self.storage.clip_prune(MAX_ITEMS, MAX_AGE_DAYS).await {
            Ok(images) => self.cleanup_media(images),
            Err(e) => warn!(error = %e, "clip prune failed"),
        }
    }

    fn cleanup_media(&self, paths: Vec<String>) {
        for path in paths {
            if let Err(e) = self.driver.remove_media(&path) {
                warn!(error = %e, path, "remove clip media failed");
            }
        }
    }
}
