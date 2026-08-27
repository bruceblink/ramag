//! 列宽估算 + 拖拽 + 单元格编辑器 + 数值列检测 + 排序比较 + Hsla.opacity 扩展

use gpui::{
    AnyElement, AppContext as _, Context, DragMoveEvent, IntoElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::input::InputState;
use ramag_domain::entities::{MAX_SQL_QUERY_BYTES, QueryResult, Value};

use crate::views::result_panel::ResultPanel;
use crate::views::result_value::display_cell_value;

pub(super) fn estimate_col_width(
    ci: usize,
    columns: &[String],
    column_types: &[String],
    result: &QueryResult,
    row_indices: &[usize],
) -> gpui::Pixels {
    const MIN_W: f32 = 100.0;
    const MAX_W: f32 = 380.0;
    const PER_CHAR: f32 = 7.5;
    const PADDING: f32 = 28.0;

    let col_chars = columns.get(ci).map(|s| s.chars().count()).unwrap_or(0);
    // 类型副标：列名 + gap(≈1 字符) + 类型字符；保证字段名永不被截断
    let type_chars = column_types
        .get(ci)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().count() + 1)
        .unwrap_or(0);
    let header_chars = col_chars + type_chars;

    let mut max_chars = header_chars;
    // 预览按 60 字符截断，与渲染保持一致。
    for &ri in row_indices.iter().take(100) {
        let Some(row) = result.rows.get(ri) else {
            continue;
        };
        if let Some(v) = row.values.get(ci) {
            let chars = display_cell_value(Some(v), 60).chars().count();
            if chars > max_chars {
                max_chars = chars;
            }
        }
    }
    let est = max_chars as f32 * PER_CHAR + PADDING;
    px(est.clamp(MIN_W, MAX_W))
}

/// 列宽拖拽 drag value：携带列索引（被拖动的列）
#[derive(Clone)]
pub(super) struct ColResizeDrag(pub usize);

impl gpui::Render for ColResizeDrag {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// 表头每列右边缘的拖拽 handle（4px 宽，cursor-col-resize）
pub(super) fn render_col_resize_handle(ci: usize, cx: &mut Context<ResultPanel>) -> AnyElement {
    div()
        .id(SharedString::from(format!("col-resize-{ci}")))
        .absolute()
        .right_0()
        .top_0()
        .h(px(34.0))
        .w(px(4.0))
        .cursor_col_resize()
        .on_drag(ColResizeDrag(ci), |drag, _pos, _, cx| {
            cx.new(|_| drag.clone())
        })
        .on_drag_move(
            cx.listener(move |this, e: &DragMoveEvent<ColResizeDrag>, _, cx| {
                let drag = e.drag(cx);
                let mouse_x = e.event.position.x;
                let handle_right = e.bounds.right();
                let delta = mouse_x - handle_right;
                if delta == px(0.0) {
                    return;
                }
                let cur = this.col_width_override(drag.0).unwrap_or_else(|| px(180.0));
                let new_w = (cur + delta).max(px(60.0)).min(px(800.0));
                this.set_col_width_override(drag.0, new_w);
                cx.notify();
            }),
        )
        .into_any_element()
}

pub(super) fn detect_numeric_column(
    ci: usize,
    result: &QueryResult,
    row_indices: &[usize],
) -> bool {
    let mut has_num = false;
    let mut all_num = true;
    for &ri in row_indices.iter().take(20) {
        let Some(row) = result.rows.get(ri) else {
            continue;
        };
        if let Some(v) = row.values.get(ci) {
            match v {
                Value::Null => {}
                Value::Int(_) | Value::Float(_) => has_num = true,
                _ => {
                    all_num = false;
                    break;
                }
            }
        }
    }
    has_num && all_num
}

/// 可写单元格直接切换为行内输入；只读单元格继续用弹框查看完整内容。
pub(super) fn open_cell_editor(
    panel: &mut ResultPanel,
    ri: usize,
    ci: usize,
    window: &mut gpui::Window,
    cx: &mut Context<ResultPanel>,
) {
    let Some((col_name, initial_text, truncated)) = panel.cell_info(ri, ci) else {
        return;
    };
    // 写入闸门未过（非单表 / 无定位键 / 生产只读 / 视图）或二进制单元格：
    // 弹框仍打开供查看 / 复制完整内容，但禁用提交并说明原因
    let read_only_reason = if truncated {
        Some(format!(
            "单元格内容超过 {} MiB 编辑上限，当前仅显示开头部分；请用 SQL 或导出处理完整值",
            MAX_SQL_QUERY_BYTES / 1024 / 1024
        ))
    } else {
        panel.cell_edit_block_reason(ri, ci)
    };
    if read_only_reason.is_none() {
        panel.begin_cell_edit(ri, ci, initial_text, window, cx);
        return;
    }
    let locate_label = panel.identity_label();
    let input = cx.new(|cx_inner| {
        InputState::new(window, cx_inner)
            .multi_line(true)
            .rows(8)
            .default_value(initial_text)
    });
    ramag_ui::enforce_multiline_input_byte_limit(
        &input,
        MAX_SQL_QUERY_BYTES,
        window,
        cx,
        |panel, _, cx| {
            panel.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "单元格编辑最多保留 {} MiB，超出部分已截断",
                    MAX_SQL_QUERY_BYTES / 1024 / 1024
                ))
                .autohide(true),
            );
            cx.notify();
        },
    )
    .detach();
    panel.set_cell_edit_input(Some(input.clone()));
    let panel_entity = cx.entity();
    crate::views::cell_edit_dialog::open(
        panel_entity,
        ri,
        ci,
        col_name,
        input,
        read_only_reason,
        locate_label,
        window,
        cx,
    );
}

/// 比较两个 Value：直接按值与类型稳定排序，不为混合列或 JSON 列生成完整字符串副本。
pub(super) fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(x), Some(y)) => match (x, y) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.total_cmp(b),
            (Value::Int(a), Value::Float(b)) => (*a as f64).total_cmp(b),
            (Value::Float(a), Value::Int(b)) => a.total_cmp(&(*b as f64)),
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::DateTime(a), Value::DateTime(b)) => a.cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::Json(a), Value::Json(b)) => compare_json(a, b),
            _ => value_sort_rank(x).cmp(&value_sort_rank(y)),
        },
    }
}

fn value_sort_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Int(_) | Value::Float(_) => 2,
        Value::Text(_) => 3,
        Value::Bytes(_) => 4,
        Value::DateTime(_) => 5,
        Value::Json(_) => 6,
    }
}

fn compare_json(left: &serde_json::Value, right: &serde_json::Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |value: &serde_json::Value| match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(_) => 2,
        serde_json::Value::String(_) => 3,
        serde_json::Value::Array(_) => 4,
        serde_json::Value::Object(_) => 5,
    };
    let rank_order = rank(left).cmp(&rank(right));
    if rank_order != Ordering::Equal {
        return rank_order;
    }
    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => left.cmp(right),
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            compare_json_numbers(left, right)
        }
        (serde_json::Value::String(left), serde_json::Value::String(right)) => left.cmp(right),
        (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
            compare_json_sequences(left.iter(), right.iter())
        }
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            let mut left = left.iter();
            let mut right = right.iter();
            loop {
                match (left.next(), right.next()) {
                    (Some((left_key, left_value)), Some((right_key, right_value))) => {
                        let key_order = left_key.cmp(right_key);
                        if key_order != Ordering::Equal {
                            return key_order;
                        }
                        let value_order = compare_json(left_value, right_value);
                        if value_order != Ordering::Equal {
                            return value_order;
                        }
                    }
                    (Some(_), None) => return Ordering::Greater,
                    (None, Some(_)) => return Ordering::Less,
                    (None, None) => return Ordering::Equal,
                }
            }
        }
        _ => Ordering::Equal,
    }
}

fn compare_json_sequences<'a>(
    mut left: impl Iterator<Item = &'a serde_json::Value>,
    mut right: impl Iterator<Item = &'a serde_json::Value>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    loop {
        match (left.next(), right.next()) {
            (Some(left), Some(right)) => {
                let order = compare_json(left, right);
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

fn compare_json_numbers(
    left: &serde_json::Number,
    right: &serde_json::Number,
) -> std::cmp::Ordering {
    match (left.as_i64(), left.as_u64(), right.as_i64(), right.as_u64()) {
        (Some(left), _, Some(right), _) => left.cmp(&right),
        (_, Some(left), _, Some(right)) => left.cmp(&right),
        (Some(left), _, _, Some(right)) => {
            if left < 0 {
                std::cmp::Ordering::Less
            } else {
                (left as u64).cmp(&right)
            }
        }
        (_, Some(left), Some(right), _) => {
            if right < 0 {
                std::cmp::Ordering::Greater
            } else {
                left.cmp(&(right as u64))
            }
        }
        _ => left
            .as_f64()
            .unwrap_or_default()
            .total_cmp(&right.as_f64().unwrap_or_default()),
    }
}

/// Hsla 透明度便捷调用
pub(super) trait OpacityExt {
    fn opacity(self, alpha: f32) -> Self;
}

impl OpacityExt for gpui::Hsla {
    fn opacity(mut self, alpha: f32) -> Self {
        self.a = alpha.clamp(0.0, 1.0);
        self
    }
}
