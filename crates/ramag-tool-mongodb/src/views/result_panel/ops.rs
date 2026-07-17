//! 结果区文档 DML：新增 / 删除 / 编辑。异步执行后 emit Refresh 重跑命令刷新结果。
//! toast 经 pending_notification 在下次 render 推送（与 dbclient::result_panel 同款）

use std::collections::HashSet;

use gpui::{App, ClickEvent, Context, Entity, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    v_flex,
};
use serde_json::Value;

use super::{ResultEvent, ResultPanel};
use crate::views::{
    MAX_MONGO_INTERACTIVE_INPUT_BYTES, bounded_input, estimated_json_value_bytes,
    inline_text_preview, reserve_input_bytes,
};

const MAX_INSERT_FORM_FIELDS: usize = 256;
const MAX_DELETE_BATCH_IDS: usize = 5_000;
const MAX_DELETE_BATCH_BYTES: usize = 4 * 1024 * 1024;

impl ResultPanel {
    /// 弹「新增文档」：按当前结果的字段逐项填写（对齐 dbclient 按列填）；确认后 insert_one
    pub(crate) fn open_insert_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_dml_busy {
            return self.notify_error("上一写操作尚未完成".to_string(), cx);
        }
        let Some(coll) = self.target_collection.clone() else {
            return;
        };
        // 字段模板：当前结果首个文档的顶层字段（排除 _id，让 mongo 自动生成）
        let fields: Vec<String> = self
            .docs_arc
            .as_ref()
            .and_then(|docs| docs.first())
            .and_then(|d| d.as_object())
            .map(|m| m.keys().filter(|k| k.as_str() != "_id").cloned().collect())
            .unwrap_or_default();
        // 空集合无字段模板：不再一味报错，改弹「整篇文档 JSON」输入框，支持插入首个文档
        if fields.is_empty() || fields.len() > MAX_INSERT_FORM_FIELDS {
            return self.open_raw_insert_dialog(coll, window, cx);
        }
        let inputs: Vec<(String, Entity<InputState>)> = fields
            .iter()
            .map(|f| {
                (
                    f.clone(),
                    cx.new(|c| bounded_input(window, c).placeholder("值（JSON / 文本，留空跳过）")),
                )
            })
            .collect();
        let panel = cx.entity().clone();
        let title = SharedString::from(format!("新增文档 → {}", inline_text_preview(&coll, 96)));
        window.open_dialog(cx, move |dialog, _, app| {
            let panel_cancel = panel.clone();
            let panel_on_cancel = panel.clone();
            let panel_apply = panel.clone();
            let inputs_apply = inputs.clone();
            let inputs_content = inputs.clone();
            let dml_busy = panel.read(app).doc_dml_busy;
            let cancel = Button::new("mongo-insert-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = Button::new("mongo-insert-apply")
                .primary()
                .small()
                .label("插入")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
                    // 不立即关弹框：成功经 pending_close_dialog 关闭，失败 / 校验不过保留输入
                    match collect_field_inputs(&inputs_apply, app) {
                        Ok(pairs) => {
                            panel_apply.update(app, |this, cx| this.do_insert_fields(pairs, cx));
                        }
                        Err(error) => {
                            panel_apply.update(app, |this, cx| this.notify_error(error, cx));
                        }
                    }
                });
            dialog
                .title(title.clone())
                .close_button(false)
                .on_cancel(move |_, _, app| dml_dialog_can_close(&panel_on_cancel, app))
                .width(px(520.0))
                .margin_top(px(100.0))
                .content(move |content, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    let mut col = v_flex().w_full().gap(px(10.0));
                    for (field, input) in &inputs_content {
                        col = col.child(
                            v_flex()
                                .w_full()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(SharedString::from(inline_text_preview(field, 128))),
                                )
                                .child(Input::new(input).small()),
                        );
                    }
                    content.child(col)
                })
                .footer(dialog_footer(cancel, apply))
        });
    }

    /// 表单字段组装成文档 → insert_one（留空字段跳过；值按 JSON 解析，失败当字符串）
    fn do_insert_fields(&mut self, pairs: Vec<(String, String)>, cx: &mut Context<Self>) {
        let mut map = serde_json::Map::new();
        for (field, raw) in pairs {
            if raw.trim().is_empty() {
                continue;
            }
            let val = match serde_json::from_str::<Value>(raw.trim()) {
                Ok(v) => v,
                Err(_) => Value::String(raw),
            };
            map.insert(field, val);
        }
        if map.is_empty() {
            return self.notify_error("未填写任何字段".to_string(), cx);
        }
        self.do_insert_doc(Value::Object(map), cx);
    }

    /// 空集合兜底：直接输入整篇文档 JSON → insert_one（无字段模板可依时用）
    fn open_raw_insert_dialog(
        &mut self,
        coll: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|c| {
            bounded_input(window, c)
                .multi_line(true)
                .placeholder("输入完整文档 JSON，如 {\"name\": \"alice\", \"age\": 30}")
                .default_value("{\n  \n}")
        });
        ramag_ui::enforce_multiline_input_byte_limit(
            &input,
            MAX_MONGO_INTERACTIVE_INPUT_BYTES,
            window,
            cx,
            |panel, _, cx| {
                panel.pending_notification = Some(
                    Notification::warning(format!(
                        "文档输入最多保留 {} MiB，超出部分已截断",
                        MAX_MONGO_INTERACTIVE_INPUT_BYTES / 1024 / 1024
                    ))
                    .autohide(true),
                );
                cx.notify();
            },
        )
        .detach();
        input.update(cx, |s, c| s.focus(window, c));
        let panel = cx.entity().clone();
        let title = SharedString::from(format!("新增文档 → {}", inline_text_preview(&coll, 96)));
        window.open_dialog(cx, move |dialog, _, app| {
            let panel_cancel = panel.clone();
            let panel_on_cancel = panel.clone();
            let panel_apply = panel.clone();
            let input_apply = input.clone();
            let input_content = input.clone();
            let dml_busy = panel.read(app).doc_dml_busy;
            let cancel = Button::new("mongo-rawinsert-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = Button::new("mongo-rawinsert-apply")
                .primary()
                .small()
                .label("插入")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
                    let raw = input_apply.read(app).value().to_string();
                    // 不立即关弹框：成功经 pending_close_dialog 关闭，解析失败保留输入
                    panel_apply.update(app, |this, cx| this.do_insert_raw(raw, cx));
                });
            dialog
                .title(title.clone())
                .close_button(false)
                .on_cancel(move |_, _, app| dml_dialog_can_close(&panel_on_cancel, app))
                .width(px(520.0))
                .margin_top(px(100.0))
                .content(move |content, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    content.child(
                        v_flex()
                            .w_full()
                            .gap(px(6.0))
                            .child(div().text_xs().text_color(muted).child(
                                "该集合暂无文档，直接输入整篇文档 JSON（_id 可省，自动生成）",
                            ))
                            .child(Input::new(&input_content).h(px(200.0))),
                    )
                })
                .footer(dialog_footer(cancel, apply))
        });
    }

    /// 原始 JSON 文本 → 校验为对象 → insert_one
    fn do_insert_raw(&mut self, raw: String, cx: &mut Context<Self>) {
        let doc = match serde_json::from_str::<Value>(raw.trim()) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => return self.notify_error("文档必须是 JSON 对象 {…}".to_string(), cx),
            Err(e) => return self.notify_error(format!("JSON 解析失败：{e}"), cx),
        };
        self.do_insert_doc(doc, cx);
    }

    /// 异步 insert_one；成功关弹框 + emit Refresh + toast，失败保留弹框与输入
    fn do_insert_doc(&mut self, doc: Value, cx: &mut Context<Self>) {
        // 防重入：上一提交未回包前忽略再次点击
        if self.doc_dml_busy {
            self.pending_notification =
                Some(Notification::warning("提交执行中，请稍候").autohide(true));
            cx.notify();
            return;
        }
        if let Err(error) =
            ramag_domain::entities::validate_mongo_document(&doc, "MongoDB insert document")
        {
            return self.notify_error(error.message().to_string(), cx);
        }
        let (Some(svc), Some(conf), Some(coll)) = (
            self.service.clone(),
            self.config.clone(),
            self.target_collection.clone(),
        ) else {
            return;
        };
        let db = self.database.clone();
        self.doc_dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = svc.insert_one(&conf, &db, &coll, doc).await;
            let _ = this.update(cx, |this, cx| {
                this.doc_dml_busy = false;
                if !this.dml_context_matches(&conf, &db, &coll) {
                    this.pending_notification = Some(match r {
                        Ok(id) => Notification::success(format!(
                            "已在原上下文 {db}.{coll} 插入文档 _id={id}；当前视图未自动刷新"
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("原上下文 {db}.{coll} 插入失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(id) => {
                        this.pending_close_dialog = true;
                        this.pending_notification = Some(
                            Notification::success(format!("已插入文档 _id={id}")).autohide(true),
                        );
                        cx.emit(ResultEvent::Refresh);
                    }
                    Err(e) => {
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("插入失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 同步路径错误 toast
    pub(crate) fn notify_error(&mut self, msg: String, cx: &mut Context<Self>) {
        self.pending_notification = Some(Notification::error(msg).autohide(true));
        cx.notify();
    }

    /// 弹删除确认；确认后对勾选行按 `_id` 受限分批删除。
    pub(crate) fn open_delete_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_dml_busy {
            return self.notify_error("上一写操作尚未完成".to_string(), cx);
        }
        if self.row_view_building {
            return self.notify_error("正在筛选 / 排序，请完成后再删除".to_string(), cx);
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
            let cancel = Button::new("mongo-del-cancel")
                .ghost()
                .small()
                .label("取消")
                .on_click(move |_: &ClickEvent, window, app| window.close_dialog(app));
            let apply = Button::new("mongo-del-apply")
                .danger()
                .small()
                .label("删除")
                .on_click(move |_: &ClickEvent, window, app| {
                    let ids = ids_apply.clone();
                    let started = panel_apply.update(app, |this, cx| this.do_delete_async(ids, cx));
                    if started {
                        window.close_dialog(app);
                    }
                });
            dialog
                .title(title.clone())
                .width(px(460.0))
                .margin_top(px(160.0))
                .content(move |content, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    content.child(div().text_sm().text_color(muted).child(SharedString::from(
                        format!("将从「{coll_hint}」按 _id 分批删除{hidden_hint}，操作不可撤销"),
                    )))
                })
                .footer(dialog_footer(cancel, apply))
        });
    }

    /// 异步分批执行 delete 命令；完成后 emit Refresh。
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
            return false;
        };
        let db = self.database.clone();
        let batches = delete_id_batches(ids, MAX_DELETE_BATCH_IDS, MAX_DELETE_BATCH_BYTES);
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

fn delete_id_batches(ids: Vec<Value>, max_ids: usize, max_bytes: usize) -> Vec<Vec<Value>> {
    debug_assert!(max_ids > 0);
    debug_assert!(max_bytes > 0);
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(ids.len().min(max_ids));
    let mut current_bytes = 0usize;

    for id in ids {
        let id_bytes = estimated_json_value_bytes(&id);
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
    batches
}

fn mongo_response_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .get("$numberLong")
            .and_then(Value::as_str)
            .and_then(|number| number.parse().ok())
    })
}

fn collect_field_inputs(
    inputs: &[(String, Entity<InputState>)],
    app: &App,
) -> Result<Vec<(String, String)>, String> {
    let mut total_bytes = 0usize;
    let mut pairs = Vec::with_capacity(inputs.len());
    for (field, input) in inputs {
        let state = input.read(app);
        let value = state.value();
        let added = field
            .len()
            .checked_add(value.len())
            .ok_or_else(|| "MongoDB 表单输入总长度溢出".to_string())?;
        let Some(next_bytes) = reserve_input_bytes(total_bytes, added) else {
            return Err("MongoDB 表单输入超过 4 MiB 总上限，请改用精简 JSON 或脚本".into());
        };
        total_bytes = next_bytes;
        pairs.push((field.clone(), value.to_string()));
    }
    Ok(pairs)
}

pub(super) fn dml_dialog_can_close(panel: &Entity<ResultPanel>, app: &mut gpui::App) -> bool {
    if panel.read(app).doc_dml_busy {
        panel.update(app, |this, cx| {
            this.pending_notification =
                Some(Notification::warning("提交执行中，完成后才能关闭").autohide(true));
            cx.notify();
        });
        return false;
    }
    true
}

pub(super) fn close_dialog_if_dml_idle(
    panel: &Entity<ResultPanel>,
    window: &mut Window,
    app: &mut gpui::App,
) {
    if !dml_dialog_can_close(panel, app) {
        return;
    }
    window.close_dialog(app);
}

/// 弹窗底部按钮条：右对齐「取消 + 主操作」，两个 dialog 共用同款布局
fn dialog_footer(cancel: Button, apply: Button) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_end()
        .gap(px(8.0))
        .child(cancel)
        .child(apply)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{delete_id_batches, mongo_response_u64};

    #[test]
    fn delete_batches_bound_count_and_estimated_bytes() {
        let by_count = delete_id_batches(vec![json!(1), json!(2), json!(3)], 2, usize::MAX);
        assert_eq!(by_count.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);

        let by_bytes = delete_id_batches(vec![json!("a"), json!("b"), json!("c")], 10, 130);
        assert_eq!(by_bytes.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
    }

    #[test]
    fn mongo_delete_count_accepts_int32_and_number_long() {
        assert_eq!(mongo_response_u64(Some(&json!(42))), Some(42));
        assert_eq!(
            mongo_response_u64(Some(&json!({"$numberLong": "5000000000"}))),
            Some(5_000_000_000)
        );
    }
}
