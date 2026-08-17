//! MongoDB 结果单元格编辑。
//! 仅允许可从显示文本无损恢复的 BSON 类型。

use gpui::{ClickEvent, Context, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    h_flex, input::Input, notification::Notification,
};
use serde_json::Value;

use super::{ResultEvent, ResultPanel};
use crate::views::{MAX_MONGO_INTERACTIVE_INPUT_BYTES, bounded_input, inline_text_preview};

impl ResultPanel {
    /// 打开单元格编辑弹窗。
    pub(crate) fn open_cell_edit_dialog(
        &self,
        id: Value,
        path: String,
        kind: &'static str,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if current.len() > MAX_MONGO_INTERACTIVE_INPUT_BYTES {
            return self.open_cell_dialog(path, kind, current, window, cx);
        }
        // 值可包含换行，使用多行输入框。
        let input = cx.new(|c| {
            bounded_input(window, c)
                .multi_line(true)
                .default_value(current)
        });
        ramag_ui::enforce_multiline_input_byte_limit(
            &input,
            MAX_MONGO_INTERACTIVE_INPUT_BYTES,
            window,
            cx,
            |panel, _, cx| {
                panel.pending_notification = Some(
                    Notification::warning(format!(
                        "字段输入最多保留 {} MiB，超出部分已截断",
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
        let title = SharedString::from(format!("编辑字段 {}", inline_text_preview(&path, 96)));
        window.open_dialog(cx, move |dialog, _, app| {
            let panel_cancel = panel.clone();
            let panel_on_cancel = panel.clone();
            let panel_apply = panel.clone();
            let input_apply = input.clone();
            let input_content = input.clone();
            let id_apply = id.clone();
            let path_apply = path.clone();
            let dml_busy = panel.read(app).doc_dml_busy;
            let cancel = ramag_ui::clickable_button("mongo-edit-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    super::ops::close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = ramag_ui::clickable_button("mongo-edit-apply")
                .primary()
                .small()
                .label("保存")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
                    let raw = input_apply.read(app).value().to_string();
                    let id = id_apply.clone();
                    let path = path_apply.clone();
                    // 成功后再关闭，失败时保留输入。
                    panel_apply.update(app, |this, cx| {
                        this.do_update_async(id, path, kind, raw, cx)
                    });
                });
            dialog
                .title(title.clone())
                .close_button(false)
                .on_cancel(move |_, _, app| super::ops::dml_dialog_can_close(&panel_on_cancel, app))
                .width(px(520.0))
                .margin_top(px(150.0))
                .content(move |content, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    content.child(
                        div()
                            .w_full()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .pb(px(6.0))
                                    .child("输入按 JSON 解析：123→数字、true→布尔、其它→字符串"),
                            )
                            .child(Input::new(&input_content).h(px(220.0))),
                    )
                })
                .footer(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_end()
                        .gap(px(8.0))
                        .child(cancel)
                        .child(apply),
                )
        });
    }

    /// 异步执行 `update_one`。
    fn do_update_async(
        &mut self,
        id: Value,
        path: String,
        kind: &'static str,
        raw: String,
        cx: &mut Context<Self>,
    ) {
        // 忽略提交尚未完成时的重复点击。
        if self.doc_dml_busy {
            self.pending_notification =
                Some(Notification::warning("提交执行中，请稍候").autohide(true));
            cx.notify();
            return;
        }
        if let Err(error) = ramag_domain::entities::validate_mongo_field_path(&path) {
            self.pending_notification =
                Some(Notification::error(error.message().to_string()).autohide(true));
            cx.notify();
            return;
        }
        let new_val = value_for_kind(kind, raw);
        let (Some(svc), Some(conf), Some(coll)) = (
            self.service.clone(),
            self.config.clone(),
            self.target_collection.clone(),
        ) else {
            return;
        };
        let db = self.database.clone();
        let filter = serde_json::json!({ "_id": id });
        let mut set = serde_json::Map::new();
        set.insert(path, new_val);
        let mut update = serde_json::Map::new();
        update.insert("$set".to_string(), Value::Object(set));
        let update = Value::Object(update);
        self.doc_dml_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = svc.update_one(&conf, &db, &coll, &filter, &update).await;
            let _ = this.update(cx, |this, cx| {
                this.doc_dml_busy = false;
                if !this.dml_context_matches(&conf, &db, &coll) {
                    this.pending_notification = Some(match r {
                        Ok(res) if res.affected == 0 => Notification::warning(format!(
                            "原上下文 {db}.{coll} 未匹配到文档；当前上下文已切换"
                        ))
                        .autohide(true),
                        Ok(res) => Notification::success(format!(
                            "已在原上下文 {db}.{coll} 更新 {} 条文档；当前视图未自动刷新",
                            res.affected
                        ))
                        .autohide(true),
                        Err(error) => Notification::error(
                            error.write_hint(&format!("原上下文 {db}.{coll} 更新失败")),
                        )
                        .autohide(true),
                    });
                    cx.notify();
                    return;
                }
                match r {
                    Ok(res) if res.affected == 0 => {
                        // 保留输入，便于核对后重试。
                        this.pending_notification = Some(
                            Notification::warning(
                                "未匹配到文档：该行无 _id，或当前 collection 与所选库不一致"
                                    .to_string(),
                            )
                            .autohide(true),
                        );
                    }
                    Ok(res) => {
                        this.pending_close_dialog = true;
                        this.pending_notification = Some(
                            Notification::success(format!("已更新 {} 条文档", res.affected))
                                .autohide(true),
                        );
                        cx.emit(ResultEvent::Refresh);
                    }
                    Err(e) => {
                        this.pending_notification =
                            Some(Notification::error(e.write_hint("更新失败")).autohide(true));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 仅允许可无损回写的 BSON 类型。
pub(super) fn kind_is_editable(kind: &str) -> bool {
    matches!(
        kind,
        "text" | "int" | "i" | "long" | "double" | "bool" | "null" | "oid" | "decimal"
    )
}

pub(super) fn cell_is_editable(kind: &str, text_bytes: usize) -> bool {
    kind_is_editable(kind) && text_bytes <= MAX_MONGO_INTERACTIVE_INPUT_BYTES
}

/// 按 BSON 类型恢复编辑值。
fn value_for_kind(kind: &str, raw: String) -> Value {
    match kind {
        "oid" => serde_json::json!({ "$oid": raw }),
        "date" => serde_json::json!({ "$date": raw }),
        "decimal" => serde_json::json!({ "$numberDecimal": raw }),
        // 用 Extended JSON 保留 Int64。
        "long" => serde_json::json!({ "$numberLong": raw }),
        _ => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => v,
            Err(_) => Value::String(raw),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{cell_is_editable, value_for_kind};
    use serde_json::json;

    #[test]
    fn special_kinds_wrap_extjson() {
        assert_eq!(
            value_for_kind("oid", "507f1f77bcf86cd799439011".into()),
            json!({"$oid": "507f1f77bcf86cd799439011"})
        );
        assert_eq!(
            value_for_kind("date", "2024-01-01T00:00:00Z".into()),
            json!({"$date": "2024-01-01T00:00:00Z"})
        );
        assert_eq!(
            value_for_kind("decimal", "100.50".into()),
            json!({"$numberDecimal": "100.50"})
        );
    }

    #[test]
    fn scalar_kinds_parse_json_or_string() {
        assert_eq!(value_for_kind("int", "42".into()), json!(42));
        assert_eq!(value_for_kind("bool", "true".into()), json!(true));
        assert_eq!(value_for_kind("text", "alice".into()), json!("alice"));
    }

    #[test]
    fn long_kind_wraps_numberlong() {
        assert_eq!(
            value_for_kind("long", "9999999999".into()),
            json!({ "$numberLong": "9999999999" })
        );
    }

    #[test]
    fn oversized_cells_are_read_only_even_for_roundtrippable_types() {
        assert!(cell_is_editable("text", 4));
        assert!(!cell_is_editable(
            "text",
            super::MAX_MONGO_INTERACTIVE_INPUT_BYTES + 1
        ));
        assert!(!cell_is_editable("binary", 4));
    }
}
