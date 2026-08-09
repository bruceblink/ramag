//! Key 详情的数据加载、大小估算与删除操作。

use gpui::{Context, ScrollStrategy};
use gpui_component::notification::Notification;
use ramag_domain::entities::RedisValue;
use ramag_domain::error::READ_ONLY_MESSAGE;
use tracing::{error, info};

use super::helpers::futures_join;
use super::list_delete::{
    DELETE_LIST_INDEX_SCRIPT, ListDeleteStatus, list_delete_marker, list_delete_status,
};
use super::{KeyDetailEvent, KeyDetailPanel, MAX_COLLECTION_ITEMS};

impl KeyDetailPanel {
    /// 加载某 key 的值（由 Session 在收到 KeyTreeEvent::Selected 时调用）。
    pub fn load_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.load_key_value(key, cx);
    }

    fn load_key_value(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(config) = self.config.clone() else {
            return;
        };
        self.request_seq = self.request_seq.wrapping_add(1);
        let request_seq = self.request_seq;
        self.key = Some(key.clone());
        self.value = None;
        self.ttl_ms = None;
        self.collection_total = None;
        self.value_byte_limited = false;
        self.value_memory_warning = false;
        self.loading = true;
        // 换 key 或刷新后滚动归顶归左。
        self.value_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.scalar_h_scroll
            .set_offset(gpui::Point::new(gpui::px(0.0), gpui::px(0.0)));
        self.scalar_scroll_gesture.reset();
        self.ttl_loading = true;
        self.ttl_error = None;
        self.error = None;
        self.key_size_bytes = None;
        self.estimating_size = false;
        self.size_error = None;
        self.value_view_mode = None;
        *self.scalar_cache.borrow_mut() = None;
        cx.notify();

        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let (value_result, ttl_result) = futures_join(
                service.get_value_limited(&config, db, &key, MAX_COLLECTION_ITEMS),
                service.key_ttl(&config, db, &key),
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                if !this.load_request_is_current(&config, db, &key, request_seq) {
                    return;
                }
                this.loading = false;
                this.ttl_loading = false;
                match value_result {
                    Ok(load) => {
                        this.value = Some(load.value);
                        this.collection_total = load.total;
                        this.value_byte_limited = load.byte_limited;
                        this.value_memory_warning = load.memory_warning;
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_key_value_load",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "load key value failed"
                        );
                        this.error = Some(format!("加载值失败：{error}"));
                    }
                }
                match ttl_result {
                    Ok(ttl) => {
                        this.ttl_ms = Some(ttl);
                        this.ttl_error = None;
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_key_ttl_load",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "load key TTL failed"
                        );
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
        let request_seq = self.request_seq;
        cx.notify();

        let service = self.service.clone();
        let db = self.db;
        cx.spawn(async move |this, cx| {
            let result = service.key_ttl(&config, db, &key).await;
            let _ = this.update(cx, |this, cx| {
                if !this.load_request_is_current(&config, db, &key, request_seq) {
                    return;
                }
                this.ttl_loading = false;
                match result {
                    Ok(ttl) => {
                        this.ttl_ms = Some(ttl);
                        this.ttl_error = None;
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_key_ttl_reload",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "retry key TTL failed"
                        );
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
                        info!(
                            operation = "redis_hash_field_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key_for_reload.len(),
                            field_bytes = field.len(),
                            "hash field deleted"
                        );
                        this.load_key_value(key_for_reload, cx);
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_hash_field_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key_for_reload.len(),
                            field_bytes = field.len(),
                            error = %error,
                            "delete hash field failed"
                        );
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
            self.load_key_value(key, cx);
        }
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

    pub(super) fn scalar_is_truncated(&self) -> bool {
        match (
            self.value.as_ref().and_then(RedisValue::scalar_byte_len),
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
        if self.estimating_size {
            return;
        }
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
        let request_seq = self.request_seq;
        cx.spawn(async move |this, cx| {
            let arguments = vec!["MEMORY".into(), "USAGE".into(), key.clone()];
            let result = service.execute_command(&config, db, arguments).await;
            let _ = this.update(cx, |this, cx| {
                if !this.load_request_is_current(&config, db, &key, request_seq) {
                    return;
                }
                this.estimating_size = false;
                match result {
                    Ok(RedisValue::Int(bytes)) if bytes >= 0 => {
                        this.key_size_bytes = Some(bytes as u64);
                        tracing::debug!(
                            operation = "redis_key_memory_usage",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            bytes,
                            "memory usage loaded"
                        );
                    }
                    Ok(RedisValue::Nil) => {
                        this.key_size_bytes = None;
                        tracing::debug!(
                            operation = "redis_key_memory_usage",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            "memory usage returned nil"
                        );
                    }
                    Ok(_) => {
                        error!(
                            operation = "redis_key_memory_usage",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            "memory usage returned an unexpected response"
                        );
                        this.size_error =
                            Some("MEMORY USAGE 应答异常（可能服务端不支持）".to_string());
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_key_memory_usage",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "memory usage failed"
                        );
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
                        info!(
                            operation = "redis_element_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            element = log_label,
                            "element deleted"
                        );
                        this.load_key_value(key, cx);
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_element_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            element = log_label,
                            error = %error,
                            "delete element failed"
                        );
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
                            info!(
                                operation = "redis_list_element_delete",
                                connection_id = %config.id,
                                db,
                                key_bytes = key.len(),
                                index,
                                "list element deleted by index"
                            );
                            this.load_key_value(key, cx);
                        }
                        ListDeleteStatus::Missing => {
                            this.pending_notification = Some(
                                Notification::warning(
                                    "列表已缩短，目标序号已不存在；未删除任何元素",
                                )
                                .autohide(true),
                            );
                            this.load_key_value(key, cx);
                        }
                        ListDeleteStatus::Changed => {
                            this.pending_notification = Some(
                                Notification::warning("列表内容已变化，为避免删错元素已取消操作")
                                    .autohide(true),
                            );
                            this.load_key_value(key, cx);
                        }
                        ListDeleteStatus::Unexpected => {
                            error!(
                                operation = "redis_list_element_delete",
                                connection_id = %config.id,
                                db,
                                key_bytes = key.len(),
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
                        error!(
                            operation = "redis_list_element_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "delete list element failed"
                        );
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
                        info!(
                            operation = "redis_key_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            "key deleted"
                        );
                        let removed_key = key.clone();
                        this.clear_key(cx);
                        cx.emit(KeyDetailEvent::Deleted(removed_key));
                    }
                    Err(error) => {
                        error!(
                            operation = "redis_key_delete",
                            connection_id = %config.id,
                            db,
                            key_bytes = key.len(),
                            error = %error,
                            "delete key failed"
                        );
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

    fn load_request_is_current(
        &self,
        config: &ramag_domain::entities::ConnectionConfig,
        db: u8,
        key: &str,
        request_seq: u64,
    ) -> bool {
        self.request_seq == request_seq && self.request_is_current(config, db, key)
    }
}
