//! MongoDB 文档写操作。

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

mod delete;

impl ResultPanel {
    pub(crate) fn open_insert_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_dml_busy {
            return self.notify_error("上一写操作尚未完成".to_string(), cx);
        }
        let Some(coll) = self.target_collection.clone() else {
            return;
        };
        // 使用首个文档字段作为模板，忽略 _id。
        let fields: Vec<String> = self
            .docs_arc
            .as_ref()
            .and_then(|docs| docs.first())
            .and_then(|d| d.as_object())
            .map(|m| m.keys().filter(|k| k.as_str() != "_id").cloned().collect())
            .unwrap_or_default();
        // 无模板时输入完整 JSON。
        if fields.is_empty() || fields.len() > MAX_INSERT_FORM_FIELDS {
            return self.open_raw_insert_dialog(coll, window, cx);
        }
        let inputs: Vec<(String, Entity<InputState>)> = fields
            .iter()
            .map(|f| {
                (
                    f.clone(),
                    cx.new(|c| bounded_input(window, c).placeholder("值（JSON/文本，可留空）")),
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
            let cancel = ramag_ui::clickable_button("mongo-insert-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = ramag_ui::clickable_button("mongo-insert-apply")
                .primary()
                .small()
                .label("插入")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
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
            let cancel = ramag_ui::clickable_button("mongo-rawinsert-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = ramag_ui::clickable_button("mongo-rawinsert-apply")
                .primary()
                .small()
                .label("插入")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
                    let raw = input_apply.read(app).value().to_string();
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
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("输入完整 JSON 文档（_id 可省略）"),
                            )
                            .child(Input::new(&input_content).h(px(200.0))),
                    )
                })
                .footer(dialog_footer(cancel, apply))
        });
    }

    fn do_insert_raw(&mut self, raw: String, cx: &mut Context<Self>) {
        let doc = match serde_json::from_str::<Value>(raw.trim()) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => return self.notify_error("文档必须是 JSON 对象 {…}".to_string(), cx),
            Err(e) => return self.notify_error(format!("JSON 解析失败：{e}"), cx),
        };
        self.do_insert_doc(doc, cx);
    }

    fn do_insert_doc(&mut self, doc: Value, cx: &mut Context<Self>) {
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
            tracing::warn!(
                operation = "mongo_document_insert",
                database = %self.database,
                has_service = self.service.is_some(),
                has_connection = self.config.is_some(),
                has_collection = self.target_collection.is_some(),
                "insert document skipped because execution context is unavailable"
            );
            return self.notify_error("当前连接或集合不可用，请刷新后重试".to_string(), cx);
        };
        let db = self.database.clone();
        let document_bytes = estimated_json_value_bytes(&doc);
        self.doc_dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = svc.insert_one(&conf, &db, &coll, doc).await;
            if let Err(error) = &r {
                tracing::error!(
                    operation = "mongo_document_insert",
                    connection_id = %conf.id,
                    database = %db,
                    collection = %coll,
                    document_bytes,
                    error = %error,
                    "insert document failed"
                );
            }
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

    pub(crate) fn notify_error(&mut self, msg: String, cx: &mut Context<Self>) {
        self.pending_notification = Some(Notification::error(msg).autohide(true));
        cx.notify();
    }
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
mod tests;
