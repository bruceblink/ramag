use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use ramag_domain::entities::{QueryResult, Value};
use ramag_ui::RestrictScrollToAxisExt as _;

use crate::views::result_panel::ResultPanel;
use crate::views::result_value::display_cell_value;

use super::super::DisplayView;
use super::{AlternateFrame, scroll::wrap_alternate_scroll};

/// Renders one source row vertically so wide records can be inspected field by field.
pub(super) fn render_transpose_view(
    panel: &mut ResultPanel,
    frame: Rc<AlternateFrame>,
    view: &DisplayView,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let Some(row_index) = selected_transpose_row(panel, &frame.result, view) else {
        return v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(frame.muted_fg)
            .child("当前结果没有可展示的行")
            .into_any_element();
    };
    let position = view
        .display_indices
        .iter()
        .position(|index| *index == row_index);
    let first_column = frame.visible_col_indices.first().copied();
    let mut header = h_flex()
        .w(frame.content_width)
        .h(px(38.0))
        .flex_none()
        .items_center()
        .gap_2()
        .px_3()
        .bg(frame.muted_bg.opacity(0.12))
        .border_b_1()
        .border_color(frame.border)
        .child(
            div()
                .flex_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(frame.fg)
                .child(format!(
                    "转置 · 第 {} 行",
                    frame
                        .row_number_offset
                        .saturating_add(row_index)
                        .saturating_add(1)
                )),
        );
    if let Some(position) = position {
        let panel_entity = cx.entity();
        let previous = position
            .checked_sub(1)
            .and_then(|index| view.display_indices.get(index).copied());
        let next = view
            .display_indices
            .get(position.saturating_add(1))
            .copied();
        header = header
            .child(
                ramag_ui::clickable_button("result-transpose-previous")
                    .ghost()
                    .small()
                    .label("上一行")
                    .disabled(previous.is_none())
                    .on_click({
                        let panel = panel_entity.clone();
                        move |_, _, app| {
                            if let (Some(row), Some(column)) = (previous, first_column) {
                                panel.update(app, |panel, cx| {
                                    panel.set_selected_cell(Some((row, column)));
                                    cx.notify();
                                });
                            }
                        }
                    }),
            )
            .child(
                ramag_ui::clickable_button("result-transpose-next")
                    .ghost()
                    .small()
                    .label("下一行")
                    .disabled(next.is_none())
                    .on_click({
                        let panel = panel_entity;
                        move |_, _, app| {
                            if let (Some(row), Some(column)) = (next, first_column) {
                                panel.update(app, |panel, cx| {
                                    panel.set_selected_cell(Some((row, column)));
                                    cx.notify();
                                });
                            }
                        }
                    }),
            );
    }

    let frame_for_fields = frame.clone();
    let fields = uniform_list(
        "result-transpose-fields",
        frame.visible_col_indices.len(),
        cx.processor(move |this, range: Range<usize>, _window, cx| {
            range
                .map(|index| render_transpose_field(this, &frame_for_fields, row_index, index, cx))
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(panel.uniform_scroll())
    .w(frame.content_width)
    .flex_1()
    .restrict_scroll_to_axis();
    let content = v_flex()
        .w(frame.content_width)
        .h_full()
        .child(header)
        .child(fields);
    let scroll = wrap_alternate_scroll(
        panel,
        content.into_any_element(),
        frame.content_width,
        "result-transpose-scroll",
        "result-transpose-v-scrollbar",
        "result-transpose-h-scrollbar",
        cx,
    );
    v_flex()
        .size_full()
        .min_w_0()
        .child(scroll)
        .into_any_element()
}

/// Chooses the selected visible source row, falling back to the first filtered row.
fn selected_transpose_row(
    panel: &ResultPanel,
    result: &QueryResult,
    view: &DisplayView,
) -> Option<usize> {
    panel
        .selected_cell()
        .map(|(row, _)| row)
        .filter(|row| *row < result.rows.len())
        .filter(|row| view.display_indices.contains(row))
        .or_else(|| view.display_indices.first().copied())
}

/// Renders one field from the selected source row and keeps its type/value readable.
fn render_transpose_field(
    panel: &mut ResultPanel,
    frame: &AlternateFrame,
    row_index: usize,
    visible_position: usize,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let Some(&column_index) = frame.visible_col_indices.get(visible_position) else {
        return div().into_any_element();
    };
    let Some(row) = frame.result.rows.get(row_index) else {
        return div().into_any_element();
    };
    let name = frame
        .result
        .columns
        .get(column_index)
        .cloned()
        .unwrap_or_default();
    let type_name = frame
        .result
        .column_types
        .get(column_index)
        .filter(|value| !value.is_empty())
        .cloned();
    let value = display_cell_value(row.values.get(column_index), 400);
    let selected = panel.selected_cell() == Some((row_index, column_index));
    let panel_entity = cx.entity();
    h_flex()
        .id(SharedString::from(format!(
            "result-transpose-field-{row_index}-{column_index}"
        )))
        .w(frame.content_width)
        .h(px(34.0))
        .flex_none()
        .items_center()
        .gap_2()
        .px_3()
        .bg(if selected {
            frame.accent.opacity(0.18)
        } else if visible_position.is_multiple_of(2) {
            frame.muted_bg.opacity(0.0)
        } else {
            frame.muted_bg.opacity(0.35)
        })
        .border_b_1()
        .border_color(frame.border)
        .cursor_pointer()
        .on_click(move |_: &ClickEvent, _, app| {
            panel_entity.update(app, |panel, cx| {
                panel.set_selected_cell(Some((row_index, column_index)));
                cx.notify();
            });
        })
        .child(
            div()
                .w(px(220.0))
                .flex_none()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(frame.fg)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(name),
        )
        .when_some(type_name, |this, type_name| {
            this.child(
                div()
                    .w(px(120.0))
                    .flex_none()
                    .text_xs()
                    .text_color(frame.muted_fg)
                    .child(type_name),
            )
        })
        .child(
            div()
                .min_w_0()
                .font_family(frame.mono_font.clone())
                .text_xs()
                .text_color(
                    if matches!(row.values.get(column_index), Some(Value::Null)) {
                        frame.muted_fg
                    } else {
                        frame.fg
                    },
                )
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(value),
        )
        .into_any_element()
}
