//! Redis 增量 SCAN：服务端 MATCH、代际取消与资源上限。
//! 每批校验代际、数据库和连接，切换后丢弃旧结果。

use gpui::Context;
use ramag_domain::entities::{ConnectionConfig, KeyMeta};
use tracing::{error, info, warn};

use super::{KeyTreePanel, MAX_LOADED_KEY_BYTES, MAX_LOADED_KEYS};

/// 单批 SCAN 的 COUNT hint，减少高延迟连接的往返。
const SCAN_BATCH: u32 = 5_000;
/// 达到该增量后重建 Trie，避免频繁阻塞 UI。
const REBUILD_STEP: usize = 25_000;
/// 输入停顿后下推 MATCH，避免展示旧筛选结果。
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);

impl KeyTreePanel {
    /// 从 cursor=0 按当前 MATCH 重新扫描。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.loading = true;
        self.has_loaded = false;
        self.error = None;
        self.keys.clear();
        self.seen_keys.clear();
        self.key_bytes = 0;
        self.clear_tree();
        self.last_rebuilt_count = 0;
        self.truncated = false;
        self.resource_limited = false;
        self.resume_cursor = Some(0);
        cx.notify();
        let generation = self.scan_generation;
        self.scan_next_batch(config, 0, generation, cx);
    }

    /// 停止扫描并保留已加载部分。
    pub(super) fn stop_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        if self.loading {
            self.loading = false;
            self.truncated = true;
            self.rebuild_tree();
            info!(
                operation = "redis_key_scan",
                connection_id = ?self.config.as_ref().map(|config| &config.id),
                db = self.db,
                count = self.keys.len(),
                reason = "user_cancelled",
                "key scan stopped by user"
            );
        }
        cx.notify();
    }

    /// 输入停顿后应用服务端 MATCH。
    pub(super) fn schedule_server_match(&mut self, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        if self.query.is_empty() {
            self.search_pending = false;
            if self.match_pattern.is_some() {
                self.match_pattern = None;
                self.refresh(cx);
            } else {
                cx.notify();
            }
            return;
        }
        self.search_pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_pending = false;
                this.apply_server_match(cx);
            });
        })
        .detach();
    }

    /// 应用服务端 MATCH 并重新扫描。
    pub(super) fn apply_server_match(&mut self, cx: &mut Context<Self>) {
        self.search_pending = false;
        let raw = self.search.read(cx).value().trim().to_string();
        let pattern = server_match_pattern(&raw);
        // 空闲时重复模式无需重扫。
        if pattern == self.match_pattern && !self.loading {
            cx.notify();
            return;
        }
        self.match_pattern = pattern;
        self.refresh(cx);
    }

    /// 从上次 cursor 继续扫描。
    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.resource_limited {
            return;
        }
        let (Some(config), Some(cursor)) = (self.config.clone(), self.resume_cursor) else {
            return;
        };
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.loading = true;
        self.truncated = false;
        self.error = None;
        let generation = self.scan_generation;
        cx.notify();
        self.scan_next_batch(config, cursor, generation, cx);
    }

    /// 扫描下一批并按需重建 Trie。
    fn scan_next_batch(
        &self,
        config: ConnectionConfig,
        cursor: u64,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let svc = self.service.clone();
        let db = self.db;
        let pattern = self.match_pattern.clone();
        cx.spawn(async move |this, cx| {
            let result = svc
                .scan_batch(&config, db, cursor, pattern.as_deref(), None, SCAN_BATCH)
                .await;
            let result = match result {
                Ok(mut batch) => {
                    let keys: Vec<String> =
                        batch.keys.iter().map(|meta| meta.key.clone()).collect();
                    match svc.key_types(&config, db, &keys).await {
                        Ok(types) => {
                            for (meta, key_type) in batch.keys.iter_mut().zip(types) {
                                meta.key_type = Some(key_type);
                            }
                        }
                        Err(error) => {
                            warn!(
                                operation = "redis_key_type_pipeline",
                                connection_id = %config.id,
                                db,
                                key_count = keys.len(),
                                error = %error,
                                "batch key type lookup failed; keeping keys without type badges"
                            );
                        }
                    }
                    Ok(batch)
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                let stale = this.scan_generation != generation
                    || this.db != db
                    || this.config.as_ref().map(|c| &c.id) != Some(&config.id);
                if stale {
                    return;
                }
                match result {
                    Ok(r) => {
                        this.has_loaded = true;
                        // SCAN 可能重复返回同一 key，按名称去重。
                        let mut resource_capped = false;
                        for meta in r.keys {
                            if !insert_key_with_budget(
                                &mut this.keys,
                                &mut this.seen_keys,
                                &mut this.key_bytes,
                                meta,
                            ) {
                                resource_capped = true;
                                break;
                            }
                        }
                        let done = r.cursor == 0;
                        resource_capped |= !done
                            && (this.keys.len() >= MAX_LOADED_KEYS
                                || this.key_bytes >= MAX_LOADED_KEY_BYTES);
                        this.resume_cursor = (!done && !resource_capped).then_some(r.cursor);
                        if resource_capped {
                            // 到达资源上限后不再继续扫描。
                            this.loading = false;
                            this.truncated = false;
                            this.resource_limited = true;
                            this.rebuild_tree();
                            info!(
                                operation = "redis_key_scan",
                                connection_id = %config.id,
                                count = this.keys.len(),
                                key_bytes = this.key_bytes,
                                db,
                                "key scan stopped at resource limit"
                            );
                        } else if done {
                            this.loading = false;
                            this.truncated = false;
                            this.rebuild_tree();
                            info!(
                                operation = "redis_key_scan",
                                connection_id = %config.id,
                                count = this.keys.len(),
                                db,
                                "key scan completed"
                            );
                        } else {
                            // 首批立即展示，后续按阈值重建。
                            if this.last_rebuilt_count == 0
                                || this.keys.len() - this.last_rebuilt_count >= REBUILD_STEP
                            {
                                this.rebuild_tree();
                            }
                            this.scan_next_batch(config, r.cursor, generation, cx);
                        }
                    }
                    Err(e) => {
                        error!(
                            operation = "redis_key_scan",
                            connection_id = %config.id,
                            db,
                            cursor,
                            error = %e,
                            "key scan failed"
                        );
                        this.loading = false;
                        if this.keys.is_empty() {
                            this.error = Some(format!("加载失败：{e}"));
                            this.seen_keys.clear();
                            this.clear_tree();
                            this.resume_cursor = None;
                        } else {
                            // 保留已加载数据，并从失败批次重试。
                            this.error = Some(format!(
                                "扫描中断：{e}（已保留 {} 个 key，可继续重试）",
                                this.keys.len()
                            ));
                            this.resume_cursor = Some(cursor);
                            this.truncated = true;
                            this.rebuild_tree();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn server_match_pattern(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        None
    } else if raw.contains(['*', '?', '[']) {
        Some(raw.to_string())
    } else {
        Some(format!("*{raw}*"))
    }
}

fn insert_key_with_budget(
    keys: &mut Vec<KeyMeta>,
    seen: &mut std::collections::HashSet<String>,
    key_bytes: &mut usize,
    meta: KeyMeta,
) -> bool {
    if seen.contains(&meta.key) {
        return true;
    }
    let Some(next_bytes) = key_bytes.checked_add(meta.key.len()) else {
        return false;
    };
    if keys.len() >= MAX_LOADED_KEYS || next_bytes > MAX_LOADED_KEY_BYTES {
        return false;
    }
    seen.insert(meta.key.clone());
    keys.push(meta);
    *key_bytes = next_bytes;
    true
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_LOADED_KEY_BYTES, MAX_LOADED_KEYS, insert_key_with_budget, server_match_pattern,
    };
    use ramag_domain::entities::KeyMeta;
    use std::collections::HashSet;

    #[test]
    fn plain_search_becomes_contains_match() {
        assert_eq!(server_match_pattern("user"), Some("*user*".into()));
        assert_eq!(server_match_pattern("user:*"), Some("user:*".into()));
        assert_eq!(server_match_pattern("  "), None);
    }

    #[test]
    fn key_cache_rejects_count_and_byte_overflow_without_charging_duplicates() {
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        let mut bytes = 0;

        assert!(insert_key_with_budget(
            &mut keys,
            &mut seen,
            &mut bytes,
            KeyMeta::bare("alpha"),
        ));
        assert!(insert_key_with_budget(
            &mut keys,
            &mut seen,
            &mut bytes,
            KeyMeta::bare("alpha"),
        ));
        assert_eq!(keys.len(), 1);
        assert_eq!(bytes, 5);

        bytes = MAX_LOADED_KEY_BYTES;
        assert!(!insert_key_with_budget(
            &mut keys,
            &mut seen,
            &mut bytes,
            KeyMeta::bare("beta"),
        ));

        let mut count_keys = vec![KeyMeta::bare("k"); MAX_LOADED_KEYS];
        let mut count_seen = HashSet::new();
        let mut count_bytes = MAX_LOADED_KEYS;
        assert!(!insert_key_with_budget(
            &mut count_keys,
            &mut count_seen,
            &mut count_bytes,
            KeyMeta::bare("overflow"),
        ));
    }
}
