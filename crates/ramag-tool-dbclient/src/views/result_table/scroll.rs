use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ramag_ui::RestrictScrollToAxisExt as _;

use crate::views::result_panel::ResultPanel;

/// Wraps an alternate result renderer with the same dual-axis scrolling and global scrollbar setting.
pub(super) fn wrap_alternate_scroll(
    panel: &ResultPanel,
    body: AnyElement,
    content_width: gpui::Pixels,
    horizontal_id: &'static str,
    vertical_scrollbar_id: &'static str,
    horizontal_scrollbar_id: &'static str,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let table_view = div()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id(horizontal_id)
                .debug_selector(move || horizontal_id.into())
                .size_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(panel.h_scroll())
                .child(body),
        )
        .child(
            div()
                .id(format!("{horizontal_id}-scroll-input"))
                .absolute()
                .inset_0()
                .on_scroll_wheel(cx.listener(ResultPanel::on_result_scroll)),
        )
        .child(
            div()
                .id(vertical_scrollbar_id)
                .debug_selector(move || vertical_scrollbar_id.into())
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(16.0))
                .bg(cx.theme().scrollbar)
                .child(
                    Scrollbar::vertical(panel.uniform_scroll())
                        .id(format!("{vertical_scrollbar_id}-control"))
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        );
    let horizontal_scrollbar = div()
        .id(horizontal_scrollbar_id)
        .debug_selector(move || horizontal_scrollbar_id.into())
        .flex_none()
        .w_full()
        .h(px(16.0))
        .relative()
        .bg(cx.theme().scrollbar)
        .child(
            Scrollbar::horizontal(panel.h_scroll())
                .id(format!("{horizontal_scrollbar_id}-control"))
                .scroll_size(gpui::size(content_width, px(16.0)))
                .scrollbar_show(ScrollbarShow::Always),
        );
    let show_horizontal_scrollbar =
        ramag_ui::database_result_settings(cx).show_horizontal_scrollbar;
    v_flex()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(table_view)
        .when(show_horizontal_scrollbar, |container| {
            container.child(horizontal_scrollbar)
        })
        .into_any_element()
}
