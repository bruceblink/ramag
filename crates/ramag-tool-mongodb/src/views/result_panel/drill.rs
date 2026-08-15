//! MongoDB 嵌套结果下钻。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{
    App, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement, Point,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex};
use ramag_domain::entities::{MAX_MONGO_FIELD_PATH_BYTES, validate_mongo_field_path};
use serde_json::Value;

use super::FlatTable;
use super::ResultPanel;
use super::cell::{Cell, cell_for_value, extjson_cell, value_at_path};
use super::flatten::{Column, build_flat_table_with_cancellable};
use crate::views::{estimated_json_value_bytes, inline_text_preview};

const MAX_DRILL_DOCUMENTS: usize = 50_000;
const MAX_DRILL_LEVELS: usize = 32;
const MAX_DRILL_RETAINED_BYTES: usize = 32 * 1024 * 1024;

pub(crate) struct DrillLevel {
    pub label: String,
    pub documents: Arc<Vec<Value>>,
    /// 继承的顶层文档 `_id`，用于回写定位。
    pub parent_id: Option<Value>,
    /// 从根到本层的字段路径。
    pub path_prefix: String,
    /// 数组层因缺少元素下标不可回写。
    pub editable: bool,
    /// 本层额外持有的近似字节数。
    pub owned_bytes: usize,
    /// 从根到直接父级的对象标识。
    pub ancestors: Vec<(String, Cell)>,
}

impl ResultPanel {
    pub(crate) fn is_drilled(&self) -> bool {
        self.drill_stack.len() > 1
    }

    pub(crate) fn drill_editable(&self) -> bool {
        self.drill_stack
            .last()
            .map(|l| l.editable && l.parent_id.is_some())
            .unwrap_or(false)
    }

    pub(crate) fn drill_parent_id(&self) -> Option<Value> {
        self.drill_stack.last().and_then(|l| l.parent_id.clone())
    }

    pub(crate) fn drill_full_path(&self, field: &str) -> String {
        match self.drill_stack.last() {
            Some(l) if !l.path_prefix.is_empty() => format!("{}.{}", l.path_prefix, field),
            _ => field.to_string(),
        }
    }

    pub(crate) fn reset_drill(&mut self, label: String, documents: Arc<Vec<Value>>) {
        self.drill_stack = vec![DrillLevel {
            label: inline_text_preview(&label, 96),
            documents,
            parent_id: None,
            path_prefix: String::new(),
            editable: false,
            owned_bytes: 0,
            ancestors: Vec::new(),
        }];
    }

    /// 数组按元素下钻，对象按单行下钻，标量忽略。
    pub(crate) fn drill_into(
        &mut self,
        field: String,
        source_row_idx: usize,
        row_id: Option<Value>,
        row_ident: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (value, owned_bytes) = match self.prepare_drill_value(source_row_idx, &field) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return,
            Err(message) => {
                self.pending_notification = Some(
                    gpui_component::notification::Notification::warning(message).autohide(true),
                );
                cx.notify();
                return;
            }
        };
        let value_is_object = matches!(value, Value::Object(_));
        let documents = Arc::new(match value {
            Value::Array(arr) => arr,
            Value::Object(_) => vec![value],
            _ => return,
        });
        let top = self.drill_stack.last();
        // 顶层 `_id` 跨层继承。
        let parent_id = top.and_then(|l| l.parent_id.clone()).or(row_id);
        let mut ancestors = top.map(|l| l.ancestors.clone()).unwrap_or_default();
        let from_label = top.map(|l| l.label.clone()).unwrap_or_default();
        if let Some(ident) = &row_ident {
            ancestors.push((from_label, cell_for_value(ident)));
        }
        let prefix = top.map(|l| l.path_prefix.clone()).unwrap_or_default();
        let path_prefix = next_editable_path(
            self.drill_stack.len() == 1,
            top.is_some_and(|level| level.editable),
            &prefix,
            &field,
            value_is_object,
        );
        self.drill_stack.push(DrillLevel {
            label: inline_text_preview(&field, 96),
            documents,
            parent_id,
            editable: path_prefix.is_some(),
            path_prefix: path_prefix.unwrap_or_default(),
            owned_bytes,
            ancestors,
        });
        self.apply_top_level(window, cx);
    }

    fn prepare_drill_value(
        &self,
        source_row_idx: usize,
        field: &str,
    ) -> Result<Option<(Value, usize)>, String> {
        if self.drill_stack.len() >= MAX_DRILL_LEVELS {
            return Err(format!(
                "嵌套下钻已达到 {MAX_DRILL_LEVELS} 层上限，请返回上层后继续查看"
            ));
        }
        let Some(value) = self
            .docs_arc
            .as_ref()
            .and_then(|documents| documents.get(source_row_idx))
            .and_then(|document| {
                if field == "_value" && !document.is_object() {
                    Some(document)
                } else {
                    value_at_path(document, field)
                }
            })
        else {
            return Ok(None);
        };
        // ExtJSON 包装是标量，不能拆成字面字段。
        let is_extjson_scalar = matches!(value, Value::Object(map) if extjson_cell(map).is_some());
        if is_extjson_scalar || !matches!(value, Value::Object(_) | Value::Array(_)) {
            return Ok(None);
        }
        if matches!(value, Value::Array(items) if items.len() > MAX_DRILL_DOCUMENTS) {
            return Err(format!(
                "数组包含超过 {MAX_DRILL_DOCUMENTS} 个元素，请先在查询中缩小范围"
            ));
        }

        let owned_bytes = estimated_json_value_bytes(value);
        let current_bytes = self.drill_stack.iter().fold(0usize, |total, level| {
            total.saturating_add(level.owned_bytes)
        });
        if current_bytes.saturating_add(owned_bytes) > MAX_DRILL_RETAINED_BYTES {
            return Err(format!(
                "嵌套内容超过 {} MiB 下钻内存上限，请缩小查询结果",
                MAX_DRILL_RETAINED_BYTES / 1024 / 1024
            ));
        }
        Ok(Some((value.clone(), owned_bytes)))
    }

    pub(crate) fn drill_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index + 1 >= self.drill_stack.len() {
            return;
        }
        self.drill_stack.truncate(index + 1);
        self.apply_top_level(window, cx);
    }

    fn apply_top_level(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let docs = self
            .drill_stack
            .last()
            .map(|l| l.documents.clone())
            .unwrap_or_else(|| Arc::new(Vec::new()));
        self.clear_selected_rows();
        // 列结构随层级变化；内容搜索是用户条件，返回上层后继续应用。
        self.clear_column_filter(window, cx);
        self.docs_arc = Some(docs);
        self.schedule_table_rebuild(cx);
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        self.scroll_gesture.reset();
        cx.notify();
    }

    pub(crate) fn render_breadcrumb(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let secondary = cx.theme().secondary;
        let border = cx.theme().border;
        let last = self.drill_stack.len().saturating_sub(1);

        let mut bar = h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py(px(5.0))
            .gap_1()
            .items_center()
            .bg(secondary)
            .border_b_1()
            .border_color(border)
            .text_xs();
        for (i, level) in self.drill_stack.iter().enumerate() {
            if i > 0 {
                bar = bar.child(div().text_color(muted).child(SharedString::from("›")));
            }
            let label = SharedString::from(inline_text_preview(&level.label, 96));
            if i == last {
                bar = bar.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(label),
                );
            } else {
                bar = bar.child(
                    div()
                        .id(SharedString::from(format!("mongo-bc-{i}")))
                        .cursor_pointer()
                        .text_color(muted)
                        .hover(move |s| s.text_color(fg))
                        .child(label)
                        .on_click(
                            cx.listener(move |panel, _, window, cx| panel.drill_to(i, window, cx)),
                        ),
                );
            }
        }
        let bar = bar.child(div().flex_1());
        if self.drill_editable() {
            bar.child(div().text_color(muted).child(SharedString::from("可编辑")))
        } else {
            bar.child(div().text_color(muted).child(SharedString::from("只读")))
        }
    }

    /// 按列过滤路径穿透对象和数组，返回只读钻取表。
    pub(crate) fn try_drill_path(
        &self,
        cx: &App,
    ) -> Option<(Arc<Vec<Value>>, Arc<FlatTable>, String)> {
        let path = self.parse_column_filter(cx).drill_path?;
        let level = self.drill_stack.last()?;
        let docs = &level.documents;
        const MAX_ELEMS: usize = 5000;
        // 穿透时保留每行的祖先标识。
        let base: Vec<(String, Cell)> = level.ancestors.clone();
        let mut node_label = level.label.clone();
        let mut current: Vec<(Vec<(String, Cell)>, &Value)> =
            docs.iter().map(|d| (base.clone(), d)).collect();
        for seg in path.split('.') {
            let mut next: Vec<(Vec<(String, Cell)>, &Value)> = Vec::new();
            for entry in &current {
                let anc = &entry.0;
                let v: &Value = entry.1;
                match v {
                    Value::Object(m) => {
                        let mut a = anc.clone();
                        a.push((node_label.clone(), id_cell_of(m)));
                        if let Some(c) = m.get(seg) {
                            next.push((a, c));
                        }
                    }
                    Value::Array(arr) => {
                        for el in arr {
                            if let Value::Object(m) = el {
                                let mut a = anc.clone();
                                a.push((node_label.clone(), id_cell_of(m)));
                                if let Some(c) = m.get(seg) {
                                    next.push((a, c));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            node_label = seg.to_string();
            current = next;
        }
        let mut rows: Vec<(Vec<(String, Cell)>, Value)> = Vec::new();
        for (anc, v) in current {
            if rows.len() >= MAX_ELEMS {
                break;
            }
            match v {
                Value::Array(arr) => {
                    for el in arr {
                        rows.push((anc.clone(), el.clone()));
                        if rows.len() >= MAX_ELEMS {
                            break;
                        }
                    }
                }
                other => rows.push((anc, other.clone())),
            }
        }
        if rows.is_empty() {
            return None;
        }
        let flat: Vec<Value> = rows.iter().map(|(_, v)| v.clone()).collect();
        let anc_rows: Vec<Vec<(String, Cell)>> = rows.into_iter().map(|(a, _)| a).collect();
        // 行数已受限，可同步展平。
        let mut ft =
            build_flat_table_with_cancellable(&flat, &BTreeSet::new(), &AtomicBool::new(false))
                .unwrap_or_default();
        prepend_ancestor_columns(&mut ft, &anc_rows);
        Some((Arc::new(flat), Arc::new(ft), path))
    }
}

/// 祖先标识列使用 `对象名._id`，空对象名退化为 `_id`。
pub(super) fn ancestor_id_column_name(object_name: &str) -> String {
    if object_name.is_empty() {
        "_id".to_string()
    } else {
        format!("{object_name}._id")
    }
}

/// 对象标识优先取 `_id`，其次取 `id`。
fn id_cell_of(m: &serde_json::Map<String, Value>) -> Cell {
    if let Some(v) = m.get("_id").or_else(|| m.get("id")) {
        cell_for_value(v)
    } else {
        Cell {
            text: String::new(),
            kind: "null",
        }
    }
}

/// 添加祖先标识列，跳过整列为空的层。
fn prepend_ancestor_columns(ft: &mut FlatTable, anc_rows: &[Vec<(String, Cell)>]) {
    let depth = anc_rows.iter().map(|a| a.len()).max().unwrap_or(0);
    if depth == 0 {
        return;
    }
    let empty = Cell {
        text: String::new(),
        kind: "null",
    };
    let mut lead_cols: Vec<Column> = Vec::new();
    let mut keep: Vec<usize> = Vec::new();
    for layer in 0..depth {
        let nonempty = anc_rows.iter().any(|a| {
            a.get(layer)
                .map(|(_, c)| !c.text.is_empty())
                .unwrap_or(false)
        });
        if !nonempty {
            continue;
        }
        let label = anc_rows
            .iter()
            .find_map(|a| a.get(layer))
            .map(|(l, _)| l.clone())
            .unwrap_or_default();
        let kind = anc_rows
            .iter()
            .filter_map(|a| a.get(layer))
            .find(|(_, c)| c.kind != "null")
            .map(|(_, c)| c.kind)
            .unwrap_or("text");
        lead_cols.push(Column {
            path: ancestor_id_column_name(&label),
            kind,
        });
        keep.push(layer);
    }
    if lead_cols.is_empty() {
        return;
    }
    let lead_rows: Vec<Vec<Cell>> = anc_rows
        .iter()
        .map(|a| {
            keep.iter()
                .map(|&l| {
                    a.get(l)
                        .map(|(_, c)| c.clone())
                        .unwrap_or_else(|| empty.clone())
                })
                .collect()
        })
        .collect();
    ft.prepend_lead(lead_cols, lead_rows);
}

fn next_editable_path(
    parent_is_root: bool,
    parent_editable: bool,
    prefix: &str,
    field: &str,
    value_is_object: bool,
) -> Option<String> {
    if !value_is_object || (!parent_is_root && !parent_editable) {
        return None;
    }
    let bytes = prefix
        .len()
        .checked_add(usize::from(!prefix.is_empty()))?
        .checked_add(field.len())?;
    if bytes > MAX_MONGO_FIELD_PATH_BYTES {
        return None;
    }
    let path = if prefix.is_empty() {
        field.to_string()
    } else {
        format!("{prefix}.{field}")
    };
    validate_mongo_field_path(&path).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::super::flatten::build_flat_table_with;
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_path_does_not_recover_after_read_only_array_level() {
        assert_eq!(
            next_editable_path(true, false, "", "profile", true).as_deref(),
            Some("profile")
        );
        assert_eq!(next_editable_path(true, false, "", "items", false), None);
        assert_eq!(next_editable_path(false, false, "", "nested", true), None);
    }

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => unreachable!("expect object"),
        }
    }

    #[test]
    fn id_cell_prefers_id_then_id_field() {
        let c = id_cell_of(&obj(
            json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}, "x": 1}),
        ));
        assert_eq!(c.text, "507f1f77bcf86cd799439011");
        assert_eq!(id_cell_of(&obj(json!({"id": "uuid-123"}))).text, "uuid-123");
        assert!(id_cell_of(&obj(json!({"x": 1}))).text.is_empty());
    }

    #[test]
    fn ancestor_columns_use_object_name_and_drop_empty_layer() {
        let mut ft = build_flat_table_with(&[json!({"a": 1}), json!({"a": 2})], &BTreeSet::new());
        let anc_rows = vec![
            vec![
                (
                    "root".to_string(),
                    Cell {
                        text: "t1".to_string(),
                        kind: "text",
                    },
                ),
                (
                    "mid".to_string(),
                    Cell {
                        text: String::new(),
                        kind: "null",
                    },
                ),
            ],
            vec![
                (
                    "root".to_string(),
                    Cell {
                        text: "t2".to_string(),
                        kind: "text",
                    },
                ),
                (
                    "mid".to_string(),
                    Cell {
                        text: String::new(),
                        kind: "null",
                    },
                ),
            ],
        ];
        prepend_ancestor_columns(&mut ft, &anc_rows);
        assert_eq!(ft.columns[0].path, "root._id");
        assert!(!ft.columns.iter().any(|c| c.path.starts_with("mid")));
        assert_eq!(ft.rows[0][0].text, "t1");
        assert_eq!(ft.rows[1][0].text, "t2");
    }

    #[test]
    fn ancestor_id_column_name_appends_id_suffix() {
        assert_eq!(ancestor_id_column_name("metrics"), "metrics._id");
        assert_eq!(ancestor_id_column_name(""), "_id");
    }
}
