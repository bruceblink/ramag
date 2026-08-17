//! 单行渲染：行号 + 复选框 + 各单元格；单元格点击分发（下钻 / 标量编辑 / 只读查看）。

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::h_flex;

use super::ResultPanel;
use super::cell::{Cell, clipboard_text_for_value, value_at_path};
use super::flatten::Column;
use super::table::{CELL_PREVIEW_MAX, CELL_WIDTH, ROW_HEIGHT, sanitize_inline, truncate};

#[allow(clippy::too_many_arguments)]
pub(super) fn render_row(
    checkbox: gpui::AnyElement,
    row_num_width: gpui::Pixels,
    row_idx_in_view: usize,
    source_row_idx: usize,
    cells: &[Cell],
    visible_cols: &[usize],
    columns: &[Column],
    fg: Hsla,
    muted: Hsla,
    // 嵌套摘要使用蓝色，提示可下钻。
    nested_fg: Hsla,
    border: Hsla,
    muted_bg: Hsla,
    mono_font: SharedString,
    allow_edit: bool,
    // 只读钻取的源文档；双击仅查看完整内容。
    drill_doc: Option<&serde_json::Value>,
    cx: &mut Context<ResultPanel>,
) -> gpui::AnyElement {
    // 斑马纹。
    let stripe = if row_idx_in_view.is_multiple_of(2) {
        muted_bg.opacity(0.0)
    } else {
        muted_bg.opacity(0.35)
    };

    let row_num_cell = div()
        .w(row_num_width)
        .flex_none()
        .h_full()
        .px_2()
        .text_xs()
        .font_family(mono_font.clone())
        .text_color(muted)
        .border_r_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_end()
        .child(SharedString::from((row_idx_in_view + 1).to_string()));

    let mut row = h_flex()
        .id(SharedString::from(format!("mongo-row-{row_idx_in_view}")))
        .h(px(ROW_HEIGHT))
        .items_center()
        .bg(stripe)
        .border_b_1()
        .border_color(border)
        .cursor_pointer()
        .child(checkbox)
        .child(row_num_cell);

    for &ci in visible_cols {
        let cell = &cells[ci];
        let column = &columns[ci];
        // 保留原值；仅清洗单行预览，避免 GPUI 断言失败。
        let preview = sanitize_inline(&truncate(&cell.text, CELL_PREVIEW_MAX));
        let is_null = cell.kind == "null" && preview.is_empty();
        let is_right = matches!(column.kind, "int" | "long" | "double" | "decimal");
        let mf = mono_font.clone();
        let path_for_click = column.path.clone();
        let kind_for_click = column.kind;
        let is_nested = matches!(cell.kind, "object" | "array");
        let column_index = ci;
        // 只读钻取预取完整内容供双击查看。
        let drill_click_text: Option<String> = drill_doc.map(|doc| {
            if is_nested {
                value_at_path(doc, column.path.as_str())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| cell.text.clone())
            } else {
                cell.text.clone()
            }
        });
        let fallback_for_copy = cell.text.clone();
        let copy_path = column.path.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "mongo-cell-{row_idx_in_view}-{ci}"
                )))
                .w(px(CELL_WIDTH))
                .flex_none()
                .h_full()
                .border_r_1()
                .border_color(border)
                .overflow_hidden()
                .cursor_pointer()
                .on_click({
                    cx.listener(move |panel, e: &gpui::ClickEvent, window, cx| {
                        if ramag_ui::is_primary_modifier_double_click(e) {
                            let text = if let Some(text) = drill_click_text.clone() {
                                if is_nested {
                                    serde_json::from_str::<serde_json::Value>(&text)
                                        .map(|value| clipboard_text_for_value(&value))
                                        .unwrap_or(text)
                                } else {
                                    text
                                }
                            } else {
                                panel
                                    .docs_arc
                                    .as_ref()
                                    .and_then(|documents| documents.get(source_row_idx))
                                    .and_then(|document| value_at_path(document, &copy_path))
                                    .map(clipboard_text_for_value)
                                    .unwrap_or_else(|| fallback_for_copy.clone())
                            };
                            ramag_ui::copy_text_with_notification(text, window, cx);
                            return;
                        }
                        if e.click_count() < 2 {
                            return;
                        }
                        // 只读钻取仅查看完整内容，不编辑或继续下钻。
                        if let Some(text) = drill_click_text.clone() {
                            panel.open_cell_dialog(
                                path_for_click.clone(),
                                kind_for_click,
                                text,
                                window,
                                cx,
                            );
                            return;
                        }
                        // 仅在双击时复制定位值，避免每帧复制大 _id。
                        let (id_for_click, ident_for_click) = panel
                            .docs_arc
                            .as_ref()
                            .and_then(|documents| documents.get(source_row_idx))
                            .map(|document| {
                                (
                                    document.get("_id").cloned(),
                                    document.get("_id").or_else(|| document.get("id")).cloned(),
                                )
                            })
                            .unwrap_or_default();
                        let Some(text_for_click) = panel
                            .table
                            .as_ref()
                            .and_then(|table| table.rows.get(source_row_idx))
                            .and_then(|row| row.get(column_index))
                            .map(|cell| cell.text.clone())
                        else {
                            return;
                        };
                        // 嵌套值进入下钻；保留行 ID 作为回写定位上下文。
                        if is_nested {
                            panel.drill_into(
                                path_for_click.clone(),
                                source_row_idx,
                                id_for_click,
                                ident_for_click,
                                window,
                                cx,
                            );
                            return;
                        }
                        // 顶层和对象下钻层可编辑；其余视图或无法无损回写的类型仅查看。
                        if allow_edit
                            && panel.can_write()
                            && !panel.is_drilled()
                            && super::edit::cell_is_editable(kind_for_click, text_for_click.len())
                            && let Some(id) = id_for_click
                        {
                            panel.open_cell_edit_dialog(
                                id,
                                path_for_click.clone(),
                                kind_for_click,
                                text_for_click,
                                window,
                                cx,
                            );
                            return;
                        }
                        if allow_edit
                            && panel.can_write()
                            && panel.drill_editable()
                            && super::edit::cell_is_editable(kind_for_click, text_for_click.len())
                            && let Some(pid) = panel.drill_parent_id()
                        {
                            panel.open_cell_edit_dialog(
                                pid,
                                panel.drill_full_path(&path_for_click),
                                kind_for_click,
                                text_for_click,
                                window,
                                cx,
                            );
                            return;
                        }
                        panel.open_cell_dialog(
                            path_for_click.clone(),
                            kind_for_click,
                            text_for_click,
                            window,
                            cx,
                        );
                    })
                })
                .child(
                    div()
                        .w_full()
                        .h_full()
                        .px_3()
                        .flex()
                        .items_center()
                        .when(is_right, |this| this.justify_end())
                        .text_xs()
                        .font_family(mf)
                        .text_color(if is_null {
                            muted
                        } else if is_nested {
                            nested_fg
                        } else {
                            fg
                        })
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(if is_null {
                            "NULL".to_string()
                        } else {
                            preview
                        })),
                ),
        );
    }
    row.into_any_element()
}
