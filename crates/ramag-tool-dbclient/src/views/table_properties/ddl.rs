//! 表属性中的只读 DDL 预览。

use gpui::{
    AnyElement, ClickEvent, IntoElement, ParentElement, ScrollHandle, Styled, StyledText, Window,
    div, prelude::*, px,
};
use gpui_component::{
    IconName, Sizable as _, Theme,
    button::ButtonVariants as _,
    h_flex,
    highlighter::{HighlightTheme, SyntaxHighlighter},
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ropey::Rope;

const DDL_MIN_WIDTH: f32 = 720.0;
const DDL_MAX_WIDTH: f32 = 3_200.0;
const DDL_CHAR_WIDTH: f32 = 7.2;

/// Renders the read-only database definition with independent scroll axes.
/// A fixed content width preserves indentation and makes long clauses reachable by horizontal scroll.
pub(super) fn render_ddl(
    loading: bool,
    ddl: Option<String>,
    error: Option<String>,
    vertical_scroll: &ScrollHandle,
    horizontal_scroll: &ScrollHandle,
    theme: &Theme,
) -> AnyElement {
    if loading {
        return centered_message("正在读取建表语句…", theme.muted_foreground);
    }
    let Some(ddl) = ddl else {
        return if let Some(error) = error {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(div().text_sm().text_color(theme.danger).child(error))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("可点击右上角刷新按钮重试"),
                )
                .into_any_element()
        } else {
            centered_message("暂无可显示的建表语句", theme.muted_foreground)
        };
    };

    let content_width = ddl_content_width(&ddl);
    let highlighted = highlight_sql(ddl.clone(), &theme.highlight_theme);
    let copy_text = ddl;
    let code = div()
        .w(px(content_width))
        .min_h_0()
        .p(px(14.0))
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .text_color(theme.foreground)
        .whitespace_nowrap()
        .child(highlighted);

    v_flex()
        .size_full()
        .min_h_0()
        .gap(px(8.0))
        .child(
            h_flex()
                .w_full()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("数据库返回的只读定义，不会直接修改表结构"),
                )
                .child(
                    ramag_ui::clickable_button("table-properties-copy-ddl")
                        .ghost()
                        .small()
                        .icon(IconName::Copy)
                        .tooltip("复制 DDL")
                        .on_click(move |_: &ClickEvent, window: &mut Window, cx| {
                            ramag_ui::copy_text_with_notification(copy_text.clone(), window, cx);
                        }),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .border_1()
                .border_color(theme.border)
                .rounded(px(5.0))
                .bg(theme.background)
                .child(
                    div()
                        .id("table-properties-ddl-scroll")
                        .size_full()
                        .overflow_x_scroll()
                        .track_scroll(horizontal_scroll)
                        .child(
                            div()
                                .id("table-properties-ddl-vertical-scroll")
                                .w(px(content_width))
                                .h_full()
                                .overflow_y_scroll()
                                .track_scroll(vertical_scroll)
                                .child(code),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom(px(16.0))
                        .right_0()
                        .w(px(14.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::vertical(vertical_scroll)
                                .id("table-properties-ddl-v-scrollbar")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(16.0))
                        .bg(theme.scrollbar)
                        .child(
                            Scrollbar::horizontal(horizontal_scroll)
                                .id("table-properties-ddl-h-scrollbar")
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                ),
        )
        .into_any_element()
}

fn centered_message(message: &'static str, color: gpui::Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(color)
        .child(message)
        .into_any_element()
}

fn ddl_content_width(ddl: &str) -> f32 {
    let longest_line = ddl
        .lines()
        .map(|line| line.chars().count() as f32)
        .fold(0.0, f32::max);
    (longest_line * DDL_CHAR_WIDTH + 28.0).clamp(DDL_MIN_WIDTH, DDL_MAX_WIDTH)
}

fn highlight_sql(sql: String, theme: &HighlightTheme) -> StyledText {
    let mut highlighter = SyntaxHighlighter::new("sql");
    highlighter.update(None, &Rope::from_str(&sql), None);
    let highlights = highlighter.styles(&(0..sql.len()), theme);
    StyledText::new(sql).with_highlights(highlights)
}

#[cfg(test)]
mod tests {
    use super::ddl_content_width;

    #[test]
    fn ddl_width_is_bounded_and_preserves_short_definition_padding() {
        assert_eq!(ddl_content_width("CREATE TABLE users (id int);"), 720.0);
        assert_eq!(ddl_content_width(&"x".repeat(1_000)), 3_200.0);
    }
}
