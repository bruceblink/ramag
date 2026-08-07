use super::*;

impl ClipboardService {
    pub async fn capture_tick(&self, settings: &ClipboardSettings) -> Result<bool> {
        if !settings.enabled {
            return Ok(false);
        }
        let count = self.driver.change_count();
        // 跳过自身写回，避免重复记录。
        if count == self.driver.own_change_count() {
            return Ok(false);
        }
        let Some(captured) = self.driver.read()? else {
            return Ok(false);
        };
        let source = self.driver.frontmost_app();

        match decide_capture(&captured, settings) {
            CaptureDecision::Skip(reason) => {
                debug!(reason, "clipboard capture skipped");
                Ok(false)
            }
            CaptureDecision::Record { hash, kind } => {
                let _guard = self.history_mutation_lock.lock().await;
                self.record(captured, kind, hash, source, settings).await
            }
        }
    }

    pub(super) async fn record(
        &self,
        mut captured: CapturedClip,
        kind: ClipKind,
        hash: String,
        source: Option<ClipSource>,
        settings: &ClipboardSettings,
    ) -> Result<bool> {
        let now = Utc::now();
        let primary_hash = hash;
        let mut content_hash = primary_hash.clone();
        if let Some(mut existing) = self.storage.clip_find_by_hash(&content_hash).await? {
            if self.payload_matches(&existing, &captured, kind).await {
                existing.last_used_at = now;
                if let Some(src) = source.clone() {
                    existing.source = Some(src);
                }
                self.storage.clip_save(&existing).await?;
                self.cache_upsert(existing);
                self.bump();
                return Ok(true);
            }

            warn!(
                clip_id = %existing.id,
                hash = %content_hash,
                "clipboard content hash collision detected"
            );
            content_hash = collision_hash(&captured, &primary_hash);
        }

        // 同一碰撞内容后续仍命中自己的二级哈希，避免每次复制都新增一条。
        if content_hash != primary_hash
            && let Some(mut existing) = self.storage.clip_find_by_hash(&content_hash).await?
            && self.payload_matches(&existing, &captured, kind).await
        {
            existing.last_used_at = now;
            if let Some(src) = source.clone() {
                existing.source = Some(src);
            }
            self.storage.clip_save(&existing).await?;
            self.cache_upsert(existing);
            self.bump();
            return Ok(true);
        }

        // 先受限解码图片，再加密保存原图和缩略图。
        let image_png = captured.image_png.take().map(Arc::new);
        let (image_path, thumb_path) = match (&image_png, settings.capture_images) {
            (Some(png), true) => {
                let thumb = match make_thumbnail_off_thread(png.clone()).await {
                    Ok(thumb) => thumb,
                    Err(error) => {
                        warn!(error = %error, "invalid clipboard image ignored");
                        return Ok(false);
                    }
                };
                let enc_full = self.storage.seal(png.as_slice()).await?;
                let full = self
                    .persist_media(format!("{content_hash}.img"), enc_full)
                    .await?;
                let thumb_result = async {
                    let enc_thumb = self.storage.seal(&thumb).await?;
                    self.persist_media(format!("{content_hash}.thumb"), enc_thumb)
                        .await
                }
                .await;
                let thumb_path = match thumb_result {
                    Ok(path) => path,
                    Err(error) => {
                        let rollback = self.unprotected_staged_media(vec![full.clone()]);
                        if let Err(cleanup_error) = self.cleanup_media(rollback).await {
                            warn!(
                                error = %cleanup_error,
                                path = %full,
                                stage = "thumbnail",
                                "rollback clipboard image failed"
                            );
                        }
                        return Err(error);
                    }
                };
                (Some(full), Some(thumb_path))
            }
            _ => (None, None),
        };
        let byte_size = if let Some(png) = &image_png {
            png.len() as u64
        } else if let Some(text) = &captured.text {
            text.len()
                .saturating_add(captured.rtf.as_ref().map_or(0, Vec::len)) as u64
        } else {
            captured.files.iter().map(String::len).sum::<usize>() as u64
        };
        let preview = make_preview(
            kind,
            captured.text.as_deref(),
            &captured.files,
            captured.image_dims,
        );

        let item = ClipItem {
            id: ClipId::new(),
            kind,
            text: captured.text,
            rtf: captured.rtf,
            image_path,
            thumb_path,
            image_dims: captured.image_dims,
            files: captured.files,
            preview,
            source,
            byte_size,
            content_hash,
            created_at: now,
            last_used_at: now,
        };
        if let Err(error) = self.storage.clip_save(&item).await {
            let staged_media = self.unprotected_staged_media(
                [&item.image_path, &item.thumb_path]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect(),
            );
            if let Err(cleanup_error) = self.cleanup_media(staged_media).await {
                warn!(
                    error = %cleanup_error,
                    clip_id = %item.id,
                    stage = "record_save",
                    "rollback clipboard media failed"
                );
            }
            return Err(error);
        }
        self.protect_item_media(&item);
        self.cache_upsert(item);
        self.prune().await;
        self.bump();
        Ok(true)
    }

    pub(super) async fn payload_matches(
        &self,
        existing: &ClipItem,
        captured: &CapturedClip,
        kind: ClipKind,
    ) -> bool {
        if existing.kind != kind {
            return false;
        }
        if !matches!(kind, ClipKind::Image) {
            return inline_payload_matches(existing, captured, kind);
        }

        let Some(expected) = captured.image_png.as_deref() else {
            return false;
        };
        if existing.byte_size != expected.len() as u64 || existing.image_dims != captured.image_dims
        {
            return false;
        }
        match self.load_image(existing).await {
            Ok(Some(actual)) => actual == expected,
            Ok(None) => false,
            Err(error) => {
                warn!(error = %error, clip_id = %existing.id, "verify clipboard image hash failed");
                false
            }
        }
    }
}
