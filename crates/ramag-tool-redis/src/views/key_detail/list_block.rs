//! List 块：uniform_list 行级虚拟化（等高行），每行带删除按钮

use std::ops::Range;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, UniformListScrollHandle,
    div, prelude::*, px, uniform_list,
};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex};
use ramag_domain::entities::{MAX_REDIS_COMMAND_ARG_BYTES, RedisValue};

use super::{KeyDetailEvent, KeyDetailPanel};

/// 行高固定 32px：uniform_list 行级虚拟化要求等高
const ROW_H: f32 = 32.0;

pub(super) fn render_list_block(
    panel: &mut Context<KeyDetailPanel>,
    key: String,
    count: usize,
    scroll: &UniformListScrollHandle,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement + use<> {
    div()
        .debug_selector(|| "redis-list-block".into())
        .flex_col()
        .size_full()
        .min_h_0()
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .child(
            uniform_list(
                "list-rows",
                count,
                panel.processor(move |this, range: Range<usize>, _w, cx| {
                    let Some(RedisValue::List(items)) = &this.value else {
                        return Vec::new();
                    };
                    let read_only = this.is_read_only();
                    range
                        .filter_map(|i| {
                            let item = items.get(i)?;
                            Some(
                                list_row(&key, i, item, read_only, fg, muted_fg, border, cx)
                                    .into_any_element(),
                            )
                        })
                        .collect()
                }),
            )
            .track_scroll(scroll)
            .size_full(),
        )
}

#[allow(clippy::too_many_arguments)]
fn list_row(
    key: &str,
    i: usize,
    item: &RedisValue,
    read_only: bool,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
) -> impl IntoElement + use<> {
    let preview = item.display_preview(256);
    let value_is_text = matches!(item, RedisValue::Text(_));
    let raw_value = match item {
        RedisValue::Text(text) if text.len() <= MAX_REDIS_COMMAND_ARG_BYTES => Some(text.clone()),
        _ => None,
    };
    let delete_disabled = read_only || raw_value.is_none();
    let key_for_emit = key.to_string();
    let del_id = SharedString::from(format!("list-del-{i}"));
    h_flex()
        .h(px(ROW_H))
        .flex_none()
        .w_full()
        .px(px(8.0))
        .border_b_1()
        .border_color(border)
        .gap(px(8.0))
        .items_center()
        .child(
            div()
                .w(px(40.0))
                .text_xs()
                .text_color(muted_fg)
                .flex_none()
                .child(format!("{i}")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(fg)
                .font_family("monospace")
                .overflow_hidden()
                .text_ellipsis()
                .child(preview),
        )
        .child(
            ramag_ui::clickable_button(del_id)
                .ghost()
                .small()
                .icon(ramag_ui::icons::trash())
                .disabled(delete_disabled)
                .tooltip(if read_only {
                    "生产连接为只读"
                } else if raw_value.is_none() {
                    if value_is_text {
                        "元素过大，请使用脚本处理"
                    } else {
                        "二进制元素暂不支持安全删除"
                    }
                } else {
                    "删除该元素"
                })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    if let Some(value) = raw_value.clone() {
                        cx.emit(KeyDetailEvent::RequestDeleteListElement(
                            key_for_emit.clone(),
                            value,
                            i,
                        ));
                    }
                })),
        )
}
