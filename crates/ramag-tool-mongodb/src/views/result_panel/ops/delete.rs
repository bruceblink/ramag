//! MongoDB 文档批量删除。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{ClickEvent, Context, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    notification::Notification,
};
use serde_json::Value;

use super::super::{ResultEvent, ResultPanel};
use super::dialog_footer;
use crate::views::estimated_json_value_bytes;

const MAX_DELETE_BATCH_IDS: usize = 5_000;
const MAX_DELETE_BATCH_BYTES: usize = 4 * 1024 * 1024;

impl ResultPanel {
    /// 打开删除确认对话框。
    pub(crate) fn open_delete_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_dml_busy {
            return self.notify_error("上一写操作尚未完成".to_string(), cx);
        }
        if self.row_view_building {
            return self.notify_error("正在筛选 / 排序，请完成后再删除".to_string(), cx);
        }
        if self.parse_column_filter(cx).drill_path.is_some() {
            return self.notify_error("请清空路径钻取后再删除文档".to_string(), cx);
        }
        if let Some(error) = &self.row_view_error {
            return self.notify_error(format!("当前行视图不可用：{error}"), cx);
        }
        let Some(documents) = self.docs_arc.as_ref() else {
            return;
        };
        let ids: Vec<Value> = self
            .selected_rows
            .iter()
            .filter_map(|&i| documents.get(i))
            .filter_map(|d| d.get("_id").cloned())
            .collect();
        if ids.is_empty() {
            return self.notify_error("勾选的文档缺少 _id，无法删除".to_string(), cx);
        }
        let ids = Arc::new(ids);
        let n = ids.len();
        let Some((visible, rows_filtered)) = self.display_row_indices(cx) else {
            return self.notify_error("当前行视图尚未准备完成".to_string(), cx);
        };
        let hidden = if rows_filtered {
            let visible: HashSet<usize> = visible.iter().copied().collect();
            self.selected_rows
                .iter()
                .filter(|ri| !visible.contains(ri))
                .count()
        } else {
            0
        };
        let hidden_hint = if hidden > 0 {
            format!("；其中 {hidden} 个当前被筛选隐藏")
        } else {
            String::new()
        };
        let coll = self.target_collection.clone().unwrap_or_default();
        let panel = cx.entity().clone();
        let title = SharedString::from(format!("删除 {n} 个文档？"));
        window.open_dialog(cx, move |dialog, _, _| {
            let panel_apply = panel.clone();
            let ids_apply = ids.clone();
            let coll_hint = coll.clone();
            let hidden_hint = hidden_hint.clone();
            let cancel = ramag_ui::clickable_button("mongo-del-cancel")
                .ghost()
                .small()
                .label("取消")
                .on_click(move |_: &ClickEvent, window, app| window.close_dialog(app));
            let apply = ramag_ui::clickable_button("mongo-del-apply")
                .danger()
                .small()
                .label("删除")
                .on_click(move |_: &ClickEvent, window, app| {
                    // 复制 ID 后再启动删除。
                    let ids = ids_apply.as_ref().clone();
                    let started = panel_apply.update(app, |this, cx| this.do_delete_async(ids, cx));
                    if started {
                        window.close_dialog(app);
                    }
                });
            dialog
                .title(ramag_ui::closable_dialog_title(
                    "mongo-delete-close",
                    title.clone(),
                    |_, _| {},
                ))
                .close_button(false)
                .width(px(460.0))
                .margin_top(px(160.0))
                .content(move |content, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    content.child(div().text_sm().text_color(muted).child(SharedString::from(
                        format!("按 _id 分批删除「{coll_hint}」{hidden_hint}，不可撤销"),
                    )))
                })
                .footer(dialog_footer(cancel, apply))
        });
    }

    /// 异步分批删除文档。
    fn do_delete_async(&mut self, ids: Vec<Value>, cx: &mut Context<Self>) -> bool {
        if self.doc_dml_busy {
            self.pending_notification =
                Some(Notification::warning("提交执行中，请稍候").autohide(true));
            cx.notify();
            return false;
        }
        let (Some(svc), Some(conf), Some(coll)) = (
            self.service.clone(),
            self.config.clone(),
            self.target_collection.clone(),
        ) else {
            tracing::warn!(
                operation = "mongo_document_delete",
                database = %self.database,
                has_service = self.service.is_some(),
                has_connection = self.config.is_some(),
                has_collection = self.target_collection.is_some(),
                "delete documents skipped because execution context is unavailable"
            );
            self.notify_error("当前连接或集合不可用，请刷新后重试".to_string(), cx);
            return false;
        };
        let db = self.database.clone();
        let requested_documents = ids.len();
        let batches = match delete_id_batches(ids, MAX_DELETE_BATCH_IDS, MAX_DELETE_BATCH_BYTES) {
            Ok(batches) => batches,
            Err(message) => {
                self.notify_error(message, cx);
                return false;
            }
        };
        let batch_count = batches.len();
        self.doc_dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut deleted = 0u64;
            let mut failed: Option<ramag_domain::error::DomainError> = None;
            for batch in batches {
                let command = serde_json::json!({
                    "delete": &coll,
                    "deletes": [{"q": {"_id": {"$in": batch}}, "limit": 0}],
                    "ordered": true,
                });
                match svc.run_command(&conf, &db, command).await {
                    Ok(reply) => match mongo_response_u64(reply.get("n")) {
                        Some(count) => deleted = deleted.saturating_add(count),
                        None => {
                            failed = Some(ramag_domain::error::DomainError::QueryFailed(
                                "MongoDB delete 响应缺少有效的 n 字段".into(),
                            ));
                            break;
                        }
                    },
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            if let Some(error) = failed.as_ref() {
                tracing::error!(
                    operation = "mongo_document_delete",
                    connection_id = %conf.id,
                    database = %db,
                    collection = %coll,
                    requested_documents,
                    deleted_documents = deleted,
                    batch_count,
                    error = %error,
                    "delete documents failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                this.doc_dml_busy = false;
                if !this.dml_context_matches(&conf, &db, &coll) {
                    this.pending_notification = Some(match failed {
                        Some(error) => Notification::error(error.write_hint(&format!(
                            "原上下文 {db}.{coll} 删除失败（已删 {deleted} 个）"
                        )))
                        .autohide(true),
                        None => Notification::success(format!(
                            "已在原上下文 {db}.{coll} 删除 {deleted} 个文档；当前视图未自动刷新"
                        ))
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match failed {
                    Some(e) => {
                        this.pending_notification = Some(
                            Notification::error(
                                e.write_hint(&format!("删除失败（已删 {deleted} 个）")),
                            )
                            .autohide(true),
                        )
                    }
                    None => {
                        this.pending_notification = Some(
                            Notification::success(format!("已删除 {deleted} 个文档"))
                                .autohide(true),
                        );
                        cx.emit(ResultEvent::Refresh);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }
}

pub(super) fn delete_id_batches(
    ids: Vec<Value>,
    max_ids: usize,
    max_bytes: usize,
) -> Result<Vec<Vec<Value>>, String> {
    debug_assert!(max_ids > 0);
    debug_assert!(max_bytes > 0);
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(ids.len().min(max_ids));
    let mut current_bytes = 0usize;

    for id in ids {
        let id_bytes = estimated_json_value_bytes(&id);
        if id_bytes > max_bytes {
            return Err(format!(
                "MongoDB _id 估算大小超过单批 {} MiB 上限，无法安全删除",
                max_bytes / 1024 / 1024
            ));
        }
        let exceeds_count = current.len() >= max_ids;
        let exceeds_bytes = current_bytes.saturating_add(id_bytes) > max_bytes;
        if !current.is_empty() && (exceeds_count || exceeds_bytes) {
            batches.push(std::mem::take(&mut current));
            current = Vec::with_capacity(max_ids);
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(id_bytes);
        current.push(id);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

pub(super) fn mongo_response_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .get("$numberLong")
            .and_then(Value::as_str)
            .and_then(|number| number.parse().ok())
    })
}
