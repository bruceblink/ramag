use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, uniform_list,
};
use gpui_component::v_flex;
use ramag_ui::RestrictScrollToAxisExt as _;

use crate::views::result_panel::ResultPanel;

use super::{AlternateFrame, scroll::wrap_alternate_scroll};

/// Renders one result row as a compact, read-only line while retaining cell selection.
pub(super) fn render_text_view(
    panel: &mut ResultPanel,
    frame: Rc<AlternateFrame>,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let frame_for_rows = frame.clone();
    let body = uniform_list(
        "result-text-rows",
        frame.display_indices.len(),
        cx.processor(move |this, range: Range<usize>, _window, cx| {
            range
                .map(|index| render_text_row(this, &frame_for_rows, index, cx))
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(panel.uniform_scroll())
    .w(frame.content_width)
    .flex_1()
    .restrict_scroll_to_axis();
    let scroll = wrap_alternate_scroll(
        panel,
        body.into_any_element(),
        frame.content_width,
        "result-text-scroll",
        "result-text-v-scrollbar",
        "result-text-h-scrollbar",
        cx,
    );
    v_flex()
        .size_full()
        .min_w_0()
        .child(scroll)
        .into_any_element()
}

/// Formats one source row into a bounded single-line preview and preserves cell selection.
fn render_text_row(
    panel: &mut ResultPanel,
    frame: &AlternateFrame,
    index: usize,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let Some(&source_index) = frame.display_indices.get(index) else {
        return div().into_any_element();
    };
    let Some(row) = frame.result.rows.get(source_index) else {
        return div().into_any_element();
    };
    let selected = panel
        .selected_cell()
        .is_some_and(|(row_index, _)| row_index == source_index);
    let fields = frame
        .visible_col_indices
        .iter()
        .filter_map(|&column_index| {
            let name = frame.result.columns.get(column_index)?;
            let value = row
                .values
                .get(column_index)
                .map_or_else(|| "NULL".to_string(), |value| value.display_preview(180));
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>();
    let line = fields.join("  |  ");
    let first_column = frame.visible_col_indices.first().copied();
    div()
        .id(SharedString::from(format!(
            "result-text-row-{source_index}"
        )))
        .w(frame.content_width)
        .h(px(32.0))
        .flex_none()
        .px_3()
        .items_center()
        .font_family(frame.mono_font.clone())
        .text_xs()
        .text_color(frame.fg)
        .bg(if selected {
            frame.accent.opacity(0.18)
        } else if index.is_multiple_of(2) {
            frame.muted_bg.opacity(0.0)
        } else {
            frame.muted_bg.opacity(0.35)
        })
        .border_b_1()
        .border_color(frame.border)
        .overflow_hidden()
        .whitespace_nowrap()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.set_selected_cell(first_column.map(|column| (source_index, column)));
            cx.notify();
        }))
        .child(format!(
            "#{}  {}",
            frame
                .row_number_offset
                .saturating_add(index)
                .saturating_add(1),
            line
        ))
        .into_any_element()
}
