//! Key 详情的数据加载、大小估算与删除操作。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::Context;
use gpui_component::notification::Notification;
use ramag_domain::entities::RedisValue;
use ramag_domain::error::READ_ONLY_MESSAGE;
use tracing::{error, info};

use super::helpers::futures_join;
use super::{COLLECTION_PAGE_SIZE, KeyDetailEvent, KeyDetailPanel};

impl KeyDetailPanel {
    /// 加载某 key 的值（由 Session 在收到 KeyTreeEvent::Selected 时调用）。
    pub fn load_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.collection_limit = COLLECTION_PAGE_SIZE;
        self.load_key_with_limit(key, false, cx);
    }

    fn load_key_with_limit(&mut self, key: String, preserve_value: bool, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        self.key = Some(key.clone());
        if preserve_value {
            self.loading_more = true;
        } else {
            self.value = None;
            self.ttl_ms = None;
            self.collection_total = None;
            self.loading = true;
        }
        self.ttl_loading = true;
        self.ttl_error = None;
        self.error = None;
        self.key_size_bytes = None;
        self.size_error = None;
        self.value_view_mode = None;
        *self.scalar_cache.borrow_mut() = None;
        cx.notify();

        let service = self.service.clone();
        let db = self.db;
        let limit = self.collection_limit;
        cx.spawn(async move |this, cx| {
            let (value_result, ttl_result) = futures_join(
                service.get_value_limited(&config, db, &key, limit),
                service.key_ttl(&config, db, &key),
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) || this.collection_limit != limit {
                    return;
                }
                this.loading = false;
                this.loading_more = false;
                this.ttl_loading = false;
                match value_result {
                    Ok(load) => {
                        this.value = Some(load.value);
                        this.collection_total = load.total;
                    }
                    Err(error) => {
                        error!(error = %error, "load key value failed");
                        if preserve_value {
                            this.collection_limit = this
                                .value
                                .as_ref()
                                .and_then(RedisValue::len)
                                .unwrap_or(COLLECTION_PAGE_SIZE)
                                .max(COLLECTION_PAGE_SIZE);
                            this.pending_notification = Some(
                                Notification::error(format!("继续加载失败：{error}"))
                                    .autohide(true),
                            );
                        } else {
                            this.error = Some(format!("加载值失败：{error}"));
                        }
                    }
                }
                match ttl_result {
                    Ok(ttl) => {
                        this.ttl_ms = Some(ttl);
                        this.ttl_error = None;
                    }
                    Err(error) => {
                        error!(error = %error, "load key TTL failed");
                        this.ttl_error = Some(format!("PTTL 获取失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn reload_ttl(&mut self, cx: &mut Context<Self>) {
        if self.ttl_loading {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        self.ttl_loading = true;
        self.ttl_error = None;
        cx.notify();

        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let result = service.key_ttl(&config, db, &key).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) {
                    return;
                }
                this.ttl_loading = false;
                match result {
                    Ok(ttl) => {
                        this.ttl_ms = Some(ttl);
                        this.ttl_error = None;
                    }
                    Err(error) => {
                        error!(error = %error, "retry key TTL failed");
                        this.ttl_error = Some(format!("PTTL 获取失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn delete_hash_field(&mut self, field: String, cx: &mut Context<Self>) {
        if !self.guard_writable(cx) {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        let service = self.service.clone();
        let db = self.db;
        let key_for_reload = key.clone();
        let arguments = vec!["HDEL".to_string(), key, field.clone()];
        cx.spawn(async move |this, cx| {
            let result = service.execute_command(&config, db, arguments).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key_for_reload) {
                    return;
                }
                match result {
                    Ok(_) => {
                        info!(?field, "hash field deleted");
                        this.load_key_with_limit(key_for_reload, false, cx);
                    }
                    Err(error) => {
                        error!(error = %error, "delete hash field failed");
                        this.pending_notification = Some(
                            Notification::error(error.write_hint("删除字段失败")).autohide(true),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn reload_current(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.key.clone() {
            self.load_key_with_limit(key, false, cx);
        }
    }

    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.loading_more || !self.has_more() {
            return;
        }
        let Some(key) = self.key.clone() else {
            return;
        };
        self.collection_limit = self.collection_limit.saturating_add(COLLECTION_PAGE_SIZE);
        self.load_key_with_limit(key, true, cx);
    }

    pub(super) fn has_more(&self) -> bool {
        match (
            self.value.as_ref().and_then(RedisValue::len),
            self.collection_total,
        ) {
            (Some(loaded), Some(total)) => (loaded as u64) < total,
            _ => false,
        }
    }

    pub(super) fn is_read_only(&self) -> bool {
        self.config.as_ref().is_some_and(|config| config.production)
    }

    fn guard_writable(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_read_only() {
            self.pending_notification =
                Some(Notification::warning(READ_ONLY_MESSAGE).autohide(true));
            cx.notify();
            false
        } else {
            true
        }
    }

    pub(super) fn estimate_size(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        self.estimating_size = true;
        self.size_error = None;
        cx.notify();

        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let arguments = vec!["MEMORY".into(), "USAGE".into(), key.clone()];
            let result = service.execute_command(&config, db, arguments).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) {
                    return;
                }
                this.estimating_size = false;
                match result {
                    Ok(RedisValue::Int(bytes)) if bytes >= 0 => {
                        this.key_size_bytes = Some(bytes as u64);
                        info!(?key, bytes, "memory usage ok");
                    }
                    Ok(RedisValue::Nil) => {
                        this.key_size_bytes = None;
                        info!(?key, "memory usage nil (key gone)");
                    }
                    Ok(other) => {
                        error!(?other, "memory usage unexpected response");
                        this.size_error =
                            Some("MEMORY USAGE 应答异常（可能服务端不支持）".to_string());
                    }
                    Err(error) => {
                        error!(error = %error, "memory usage failed");
                        this.size_error = Some(format!("MEMORY USAGE 失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_element(
        &mut self,
        arguments: Vec<String>,
        log_label: &'static str,
        cx: &mut Context<Self>,
    ) {
        if !self.guard_writable(cx) {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let result = service.execute_command(&config, db, arguments).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) {
                    return;
                }
                match result {
                    Ok(_) => {
                        info!(label = log_label, "element deleted");
                        this.load_key_with_limit(key, false, cx);
                    }
                    Err(error) => {
                        error!(error = %error, label = log_label, "delete element failed");
                        this.pending_notification = Some(
                            Notification::error(error.write_hint("删除元素失败")).autohide(true),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn delete_list_element(&mut self, value: String, index: usize, cx: &mut Context<Self>) {
        if !self.guard_writable(cx) {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        let service = self.service.clone();
        let db = self.db;
        let arguments = vec![
            "EVAL".into(),
            DELETE_LIST_INDEX_SCRIPT.into(),
            "1".into(),
            key.clone(),
            index.to_string(),
            value,
            list_delete_marker(),
        ];
        cx.spawn(async move |this, cx| {
            let result = service.execute_command(&config, db, arguments).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) {
                    return;
                }
                match result {
                    Ok(response) => match list_delete_status(&response) {
                        ListDeleteStatus::Deleted => {
                            info!(index, "list element deleted by index");
                            this.load_key_with_limit(key, false, cx);
                        }
                        ListDeleteStatus::Missing => {
                            this.pending_notification = Some(
                                Notification::warning(
                                    "列表已缩短，目标序号已不存在；未删除任何元素",
                                )
                                .autohide(true),
                            );
                            this.load_key_with_limit(key, false, cx);
                        }
                        ListDeleteStatus::Changed => {
                            this.pending_notification = Some(
                                Notification::warning("列表内容已变化，为避免删错元素已取消操作")
                                    .autohide(true),
                            );
                            this.load_key_with_limit(key, false, cx);
                        }
                        ListDeleteStatus::Unexpected => {
                            error!(
                                ?response,
                                "delete list element returned unexpected response"
                            );
                            this.pending_notification = Some(
                                Notification::error("删除 List 元素失败：服务端应答异常")
                                    .autohide(true),
                            );
                            cx.notify();
                        }
                    },
                    Err(error) => {
                        error!(error = %error, "delete list element failed");
                        this.pending_notification = Some(
                            Notification::error(error.write_hint("删除 List 元素失败"))
                                .autohide(true),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn delete_set_element(&mut self, member: String, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        self.delete_element(vec!["SREM".into(), key, member], "srem", cx);
    }

    pub fn delete_stream_entry(&mut self, entry_id: String, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        self.delete_element(vec!["XDEL".into(), key, entry_id], "xdel", cx);
    }

    pub fn delete_zset_member(&mut self, member: String, cx: &mut Context<Self>) {
        let Some(key) = self.key.clone() else {
            return;
        };
        self.delete_element(vec!["ZREM".into(), key, member], "zrem", cx);
    }

    pub fn delete_key_now(&mut self, cx: &mut Context<Self>) {
        if !self.guard_writable(cx) {
            return;
        }
        let Some(config) = self.config.clone() else {
            return;
        };
        let Some(key) = self.key.clone() else {
            return;
        };
        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let result = service.delete_key(&config, db, &key).await;
            let _ = this.update(cx, |this, cx| {
                if !this.request_is_current(&config, db, &key) {
                    return;
                }
                match result {
                    Ok(_) => {
                        info!(?key, "key deleted");
                        let removed_key = key.clone();
                        this.clear_key(cx);
                        cx.emit(KeyDetailEvent::Deleted(removed_key));
                    }
                    Err(error) => {
                        error!(error = %error, "delete key failed");
                        this.pending_notification =
                            Some(Notification::error(error.write_hint("删除失败")).autohide(true));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn request_is_current(
        &self,
        config: &ramag_domain::entities::ConnectionConfig,
        db: u8,
        key: &str,
    ) -> bool {
        self.key.as_deref() == Some(key)
            && self.db == db
            && self.config.as_ref().map(|current| &current.id) == Some(&config.id)
    }
}

/// 在 Redis 服务器内原子校验“序号 + 旧值”后删除，避免 LREM 对重复值删错位置。
/// 返回 1=已删除，0=序号不存在，-1=该位置内容已变化。
const DELETE_LIST_INDEX_SCRIPT: &str = r#"
local current = redis.call('LINDEX', KEYS[1], ARGV[1])
if not current then return 0 end
if current ~= ARGV[2] then return -1 end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
return redis.call('LREM', KEYS[1], 1, ARGV[3])
"#;

fn list_delete_marker() -> String {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    format!("\0ramag-list-delete:{}:{nanos}:{nonce}", std::process::id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListDeleteStatus {
    Deleted,
    Missing,
    Changed,
    Unexpected,
}

fn list_delete_status(response: &RedisValue) -> ListDeleteStatus {
    match response {
        RedisValue::Int(1) => ListDeleteStatus::Deleted,
        RedisValue::Int(0) => ListDeleteStatus::Missing,
        RedisValue::Int(-1) => ListDeleteStatus::Changed,
        _ => ListDeleteStatus::Unexpected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_delete_response_is_classified_explicitly() {
        assert_eq!(
            list_delete_status(&RedisValue::Int(1)),
            ListDeleteStatus::Deleted
        );
        assert_eq!(
            list_delete_status(&RedisValue::Int(0)),
            ListDeleteStatus::Missing
        );
        assert_eq!(
            list_delete_status(&RedisValue::Int(-1)),
            ListDeleteStatus::Changed
        );
        assert_eq!(
            list_delete_status(&RedisValue::Text("1".into())),
            ListDeleteStatus::Unexpected
        );
    }

    #[test]
    fn list_delete_markers_are_unique_and_binary_prefixed() {
        let first = list_delete_marker();
        let second = list_delete_marker();
        assert_ne!(first, second);
        assert!(first.starts_with('\0'));
    }
}
