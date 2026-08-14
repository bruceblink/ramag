//! TableTreePanel 的辅助渲染 / 工具函数（从 table_tree.rs 拆出，避免单文件过大）

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::h_flex;
use ramag_domain::entities::Column;

use super::table_tree::TableTreePanel;

/// 列子节点：主键 + 列名 + NOT NULL 标记 + raw_type。长名不截断，靠外层横滚；行高 28px 配 uniform_list
pub(super) fn render_column_row(
    col: &Column,
    element_id: SharedString,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    cx: &mut Context<TableTreePanel>,
) -> AnyElement {
    let pk_label = if col.is_primary_key { "🔑 " } else { "" };
    let null_mark = if col.nullable { "" } else { " *" };
    let name_for_copy = col.name.clone();
    h_flex()
        .id(element_id)
        .h(px(28.0))
        .flex_none()
        .pl(px(56.0))
        .pr_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
            if ramag_ui::is_primary_modifier_double_click(event) {
                ramag_ui::copy_text_with_notification(name_for_copy.clone(), window, cx);
            }
        }))
        .child(
            div()
                .text_xs()
                .text_color(fg)
                .whitespace_nowrap()
                .child(format!("{}{}{}", pk_label, col.name.clone(), null_mark)),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted_fg)
                .whitespace_nowrap()
                .child(col.data_type.raw_type.clone()),
        )
        .into_any_element()
}

/// 索引、外键等详情行：显示说明文本，复制可直接用于 SQL 的对象名。
pub(super) fn render_copyable_detail_line(
    element_id: SharedString,
    text: impl Into<SharedString>,
    copy_value: String,
    color: gpui::Hsla,
    cx: &mut Context<TableTreePanel>,
) -> AnyElement {
    div()
        .id(element_id)
        .w_full()
        .h(px(28.0))
        .flex_none()
        .pl(px(56.0))
        .pr_2()
        .pt(px(6.0))
        .text_xs()
        .text_color(color)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .cursor_pointer()
        .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
            if ramag_ui::is_primary_modifier_double_click(event) {
                ramag_ui::copy_text_with_notification(copy_value.clone(), window, cx);
            }
        }))
        .child(text.into())
        .into_any_element()
}

/// 加载中 / 错误占位行：缩进同列子节点，单行 ellipsis 截断，行高 28px
pub(super) fn render_columns_placeholder(
    text: impl Into<SharedString>,
    color: gpui::Hsla,
) -> AnyElement {
    div()
        .w_full()
        .h(px(28.0))
        .flex_none()
        .pl(px(56.0))
        .pr_2()
        .pt(px(6.0))
        .text_xs()
        .text_color(color)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .child(text.into())
        .into_any_element()
}
