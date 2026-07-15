//! 单元格编辑：双击 → 输入新值 → update_one $set（dotted path）。
//! 按列原始 BSON 类型还原写入值，避免 oid/date/decimal 被降级成字符串 / 浮点。

use gpui::{ClickEvent, Context, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    notification::Notification,
};
use serde_json::Value;

use super::{ResultEvent, ResultPanel};

impl ResultPanel {
    /// 双击单元格编辑：输入新值 → update_one $set（dotted path）。
    /// kind 是该列原始 BSON 类型，用于保存时按类型还原（oid/date/decimal 不降级成字符串）
    pub(crate) fn open_cell_edit_dialog(
        &self,
        id: Value,
        path: String,
        kind: &'static str,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // multi_line：值可能含换行（GPUI 单行 shape_line 不接受 \n），且本对话框是 220px 多行编辑框
        let input = cx.new(|c| {
            InputState::new(window, c)
                .multi_line(true)
                .default_value(current)
        });
        input.update(cx, |s, c| s.focus(window, c));
        let panel = cx.entity().clone();
        let title = SharedString::from(format!("编辑字段 {path}"));
        window.open_dialog(cx, move |dialog, _, app| {
            let panel_cancel = panel.clone();
            let panel_on_cancel = panel.clone();
            let panel_apply = panel.clone();
            let input_apply = input.clone();
            let input_content = input.clone();
            let id_apply = id.clone();
            let path_apply = path.clone();
            let dml_busy = panel.read(app).doc_dml_busy;
            let cancel = Button::new("mongo-edit-cancel")
                .ghost()
                .small()
                .label("取消")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, window, app| {
                    super::ops::close_dialog_if_dml_idle(&panel_cancel, window, app);
                });
            let apply = Button::new("mongo-edit-apply")
                .primary()
                .small()
                .label("保存")
                .disabled(dml_busy)
                .on_click(move |_: &ClickEvent, _window, app| {
                    let raw = input_apply.read(app).value().to_string();
                    let id = id_apply.clone();
                    let path = path_apply.clone();
                    // 不立即关弹框：请求成功后经 pending_close_dialog 关闭，失败保留输入可改
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

    /// 异步 update_one：filter {_id} + $set {dotted path: 新值（按列 kind 还原 BSON 类型）}
    fn do_update_async(
        &mut self,
        id: Value,
        path: String,
        kind: &'static str,
        raw: String,
        cx: &mut Context<Self>,
    ) {
        // 防重入：上一提交未回包前忽略再次点击（弹框仍开着，按钮可被连点）
        if self.doc_dml_busy {
            self.pending_notification =
                Some(Notification::warning("提交执行中，请稍候").autohide(true));
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
                        // 未命中不关弹框：用户可核对 _id / 库后再试
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
                        // 失败保留弹框与输入，仅 toast 说明原因
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

/// 按列原始 BSON 类型把单元格编辑文本还原为写入值：
/// 特殊类型（oid/date/decimal）包回 Extended JSON，避免 $set 把它降级成字符串 / 浮点；
/// 该 BSON 类型的单元格是否可安全编辑。只放行能从显示文本无损还原的类型：
/// binary/code/regex/ts/symbol/dbptr/minkey/maxkey 等显示的是摘要串，编辑会把真实
/// BSON 覆盖成摘要（静默毁数据）；date 显示形态不定（canonical 为毫秒数字），
/// 回写易被 driver 拒绝，一并设为只读。
pub(super) fn kind_is_editable(kind: &str) -> bool {
    matches!(
        kind,
        "text" | "int" | "i" | "long" | "double" | "bool" | "null" | "oid" | "decimal"
    )
}

/// 其余按 JSON 解析（123→数字 / true→布尔 / 其它→字符串），保留「可改类型」的灵活性。
/// 注：date 文本需为 ISO8601（结果集 relaxed Extended JSON 形态即 ISO），否则 driver 转换报错
fn value_for_kind(kind: &str, raw: String) -> Value {
    match kind {
        "oid" => serde_json::json!({ "$oid": raw }),
        "date" => serde_json::json!({ "$date": raw }),
        "decimal" => serde_json::json!({ "$numberDecimal": raw }),
        // Int64：显式包 $numberLong 保住 64 位，否则正小值会被 serde 反序列化窄化成 Int32
        "long" => serde_json::json!({ "$numberLong": raw }),
        _ => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => v,
            Err(_) => Value::String(raw),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::value_for_kind;
    use serde_json::json;

    #[test]
    fn special_kinds_wrap_extjson() {
        // oid/date/decimal 必须包回 Extended JSON，否则会被写成普通字符串/浮点
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
        // Int64 编辑写回必须包 $numberLong，保住 64 位不被 serde 窄化成 Int32
        assert_eq!(
            value_for_kind("long", "9999999999".into()),
            json!({ "$numberLong": "9999999999" })
        );
    }
}
