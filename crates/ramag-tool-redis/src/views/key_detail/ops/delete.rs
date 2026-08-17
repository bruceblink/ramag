//! Redis Key 与集合元素删除。

use gpui::Context;
use gpui_component::notification::Notification;
use tracing::{error, info};

use super::super::list_delete::{
    DELETE_LIST_INDEX_SCRIPT, ListDeleteStatus, list_delete_marker, list_delete_status,
};
use super::super::{KeyDetailEvent, KeyDetailPanel};
use super::log_stale_request_failure;

impl KeyDetailPanel {
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
                    if let Err(error) = &result {
                        log_stale_request_failure("redis_element_delete", &config, db, &key, error);
                    }
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
                    if let Err(error) = &result {
                        log_stale_request_failure(
                            "redis_list_element_delete",
                            &config,
                            db,
                            &key,
                            error,
                        );
                    }
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
                                reason = "unexpected_delete_response",
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
                    if let Err(error) = &result {
                        log_stale_request_failure("redis_key_delete", &config, db, &key, error);
                    }
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
}
