//! 嵌套数据原地下钻：双击嵌套单元格 → 把该值当新结果集，复用大列表渲染；面包屑导航返回。
//! 下钻层只读（内嵌数据非独立 collection，编辑需回写父文档，暂不支持）。

use std::sync::Arc;

use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement, Point, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, h_flex};
use ramag_domain::entities::{MAX_MONGO_FIELD_PATH_BYTES, validate_mongo_field_path};
use serde_json::Value;

use super::ResultPanel;
use super::cell::{Cell, cell_for_value};
use crate::views::{estimated_json_value_bytes, inline_text_preview};

const MAX_DRILL_DOCUMENTS: usize = 50_000;
const MAX_DRILL_LEVELS: usize = 32;
const MAX_DRILL_RETAINED_BYTES: usize = 32 * 1024 * 1024;

/// 下钻栈一层：label 用于面包屑显示，documents 为该层文档
pub(crate) struct DrillLevel {
    pub label: String,
    pub documents: Arc<Vec<Value>>,
    /// 顶层文档 _id（回写定位用，一路继承；顶层与无 _id 时为 None）
    pub parent_id: Option<Value>,
    /// 从根到本层的 dotted 路径前缀（如 "project" / "project.sub"；顶层为空）
    pub path_prefix: String,
    /// 本层能否回写编辑：对象下钻=true，数组下钻=false（丢了元素下标）
    pub editable: bool,
    /// 本层为下钻额外克隆的近似内存；根层共享查询结果，不计入预算。
    pub owned_bytes: usize,
    /// 祖先 (对象名, id) 链（根→直接父，本层常量）：作前导列展示，列名即对象名（面包屑里的层级名）
    pub ancestors: Vec<(String, Cell)>,
}

impl ResultPanel {
    /// 是否已下钻（栈深 > 1）→ 显示面包屑（对象层可编辑，数组层只读）
    pub(crate) fn is_drilled(&self) -> bool {
        self.drill_stack.len() > 1
    }

    /// 当前下钻层可否回写编辑：对象下钻层 + 已知顶层 _id
    pub(crate) fn drill_editable(&self) -> bool {
        self.drill_stack
            .last()
            .map(|l| l.editable && l.parent_id.is_some())
            .unwrap_or(false)
    }

    /// 当前下钻层对应的顶层文档 _id（回写 filter 用）
    pub(crate) fn drill_parent_id(&self) -> Option<Value> {
        self.drill_stack.last().and_then(|l| l.parent_id.clone())
    }

    /// 下钻层裸字段 → 完整 dotted 路径（path_prefix.field）
    pub(crate) fn drill_full_path(&self, field: &str) -> String {
        match self.drill_stack.last() {
            Some(l) if !l.path_prefix.is_empty() => format!("{}.{}", l.path_prefix, field),
            _ => field.to_string(),
        }
    }

    /// 重置下钻栈为顶层（新查询时由 set_result 调）
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

    /// 双击嵌套单元格 → 下钻：数组→元素逐行；对象→单行；标量不下钻。
    /// row_id 是被下钻那一行的 _id（首次下钻=顶层文档 _id），用于记录回写定位上下文
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
        // 顶层 _id 一路继承；首次下钻栈顶是顶层文档，用其行 _id
        let parent_id = top.and_then(|l| l.parent_id.clone()).or(row_id);
        // 祖先链：继承父层，再追加"被下钻那一层"的对象名 + 该行 id（_id 或 id），列名即对象名
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
                    document.get(field)
                }
            })
        else {
            return Ok(None);
        };
        if !matches!(value, Value::Object(_) | Value::Array(_)) {
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

    /// 点面包屑第 index 层 → 截断栈并恢复该层
    pub(crate) fn drill_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index + 1 >= self.drill_stack.len() {
            return;
        }
        self.drill_stack.truncate(index + 1);
        self.apply_top_level(window, cx);
    }

    /// 栈顶 documents → 当前显示：同步文档 + 重算表格 + 清过滤 + 滚动归零
    fn apply_top_level(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let docs = self
            .drill_stack
            .last()
            .map(|l| l.documents.clone())
            .unwrap_or_else(|| Arc::new(Vec::new()));
        self.selected_rows.clear();
        // 换层清空过滤（新层新列，旧过滤无意义）→ 展开路径随之清空
        self.column_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.row_filter
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.docs_arc = Some(docs);
        // 重建基础表 + 补全源（过滤已清空）
        self.schedule_table_rebuild(cx);
        self.h_scroll.set_offset(Point::new(px(0.0), px(0.0)));
        cx.notify();
    }

    /// 面包屑栏（仅下钻后渲染）：可点段返回上层，当前层高亮，右侧「只读」提示
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
        // 对象下钻层可改字段；数组层 / 无 _id 层仍只读，右侧提示用户当前层能力
        if self.drill_editable() {
            bar.child(div().text_color(muted).child(SharedString::from("可编辑")))
        } else {
            bar.child(div().text_color(muted).child(SharedString::from("只读")))
        }
    }
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
    use super::*;

    #[test]
    fn edit_path_does_not_recover_after_read_only_array_level() {
        assert_eq!(
            next_editable_path(true, false, "", "profile", true).as_deref(),
            Some("profile")
        );
        assert_eq!(next_editable_path(true, false, "", "items", false), None);
        assert_eq!(next_editable_path(false, false, "", "nested", true), None);
    }
}
