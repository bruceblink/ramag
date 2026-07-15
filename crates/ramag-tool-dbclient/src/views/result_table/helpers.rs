//! 列宽估算 + 拖拽 + 单元格编辑器 + 数值列检测 + 排序比较 + Hsla.opacity 扩展

use gpui::{
    AnyElement, AppContext as _, Context, DragMoveEvent, IntoElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::input::InputState;
use ramag_domain::entities::{QueryResult, Value};

use crate::views::result_panel::ResultPanel;

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
    // display_preview(60) 与渲染保持一致：被截断成 60 的内容自然不会撑爆 380 上限
    for &ri in row_indices.iter().take(100) {
        let Some(row) = result.rows.get(ri) else {
            continue;
        };
        if let Some(v) = row.values.get(ci) {
            let chars = v.display_preview(60).chars().count();
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

/// 同步打开单元格编辑弹框：必须在 listener 内调（已持 ResultPanel mut ref）
pub(super) fn open_cell_editor(
    panel: &mut ResultPanel,
    ri: usize,
    ci: usize,
    window: &mut gpui::Window,
    cx: &mut Context<ResultPanel>,
) {
    let Some((col_name, initial_text)) = panel.cell_info(ri, ci) else {
        return;
    };
    // 写入闸门未过（非单表 / 无定位键 / 生产只读 / 视图）或二进制单元格：
    // 弹框仍打开供查看 / 复制完整内容，但禁用提交并说明原因
    let read_only_reason = panel.cell_edit_block_reason(ri, ci);
    let locate_label = panel.identity_label();
    let input = cx.new(|cx_inner| {
        InputState::new(window, cx_inner)
            .multi_line(true)
            .rows(8)
            .default_value(initial_text)
    });
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

/// 比较两个 Value：Null 视为最小，同型按值比较，跨型用字符串兜底
pub(super) fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    compare_values_inner(a, b, |left, right| {
        display_sort_key(left).cmp(&display_sort_key(right))
    })
}

/// 预计算展示排序键后比较，避免 JSON / 混合类型在 O(N log N) 比较中反复序列化。
pub(super) fn compare_values_with_display_keys(
    a: Option<&Value>,
    b: Option<&Value>,
    a_key: Option<&str>,
    b_key: Option<&str>,
) -> std::cmp::Ordering {
    compare_values_inner(a, b, |left, right| match (a_key, b_key) {
        (Some(left), Some(right)) => left.cmp(right),
        _ => compare_values(Some(left), Some(right)),
    })
}

fn compare_values_inner(
    a: Option<&Value>,
    b: Option<&Value>,
    fallback: impl FnOnce(&Value, &Value) -> std::cmp::Ordering,
) -> std::cmp::Ordering {
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
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Value::Int(a), Value::Float(b)) => {
                (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (Value::Float(a), Value::Int(b)) => {
                a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal)
            }
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::DateTime(a), Value::DateTime(b)) => a.cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            _ => fallback(x, y),
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectSortKind {
    Bool,
    Numeric,
    Text,
    Bytes,
    DateTime,
}

/// 同列只有直接可比类型时无需展示键；JSON 或混合类型才预计算一次完整字符串。
pub(super) fn needs_display_sort_keys<'a>(
    values: impl IntoIterator<Item = Option<&'a Value>>,
) -> bool {
    let mut kind = None;
    for value in values.into_iter().flatten() {
        let current = match value {
            Value::Null => continue,
            Value::Bool(_) => DirectSortKind::Bool,
            Value::Int(_) | Value::Float(_) => DirectSortKind::Numeric,
            Value::Text(_) => DirectSortKind::Text,
            Value::Bytes(_) => DirectSortKind::Bytes,
            Value::DateTime(_) => DirectSortKind::DateTime,
            Value::Json(_) => return true,
        };
        if kind.is_some_and(|previous| previous != current) {
            return true;
        }
        kind = Some(current);
    }
    false
}

/// 与 `display_preview(usize::MAX)` 等价，但避免 Text / JSON 先完整复制再二次复制。
pub(super) fn display_sort_key(value: &Value) -> String {
    match value {
        Value::Text(text) if text.contains(['\n', '\r']) => text.replace(['\n', '\r'], " "),
        Value::Text(text) => text.clone(),
        Value::Json(json) => json.to_string(),
        other => other.display_preview(usize::MAX),
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
