//! Set 块：uniform_list 行级虚拟化（等高行），每行带删除按钮

use std::ops::Range;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, UniformListScrollHandle,
    div, px, uniform_list,
};
use gpui_component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use ramag_domain::entities::{MAX_REDIS_COMMAND_ARG_BYTES, RedisValue};

use super::{KeyDetailEvent, KeyDetailPanel};

/// 行高固定 32px：uniform_list 行级虚拟化要求等高
const ROW_H: f32 = 32.0;

pub(super) fn render_set_block(
    panel: &mut Context<KeyDetailPanel>,
    key: String,
    count: usize,
    scroll: &UniformListScrollHandle,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .min_h_0()
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .child(
            uniform_list(
                "set-rows",
                count,
                panel.processor(move |this, range: Range<usize>, _w, cx| {
                    let Some(RedisValue::Set(items)) = &this.value else {
                        return Vec::new();
                    };
                    let read_only = this.is_read_only();
                    range
                        .filter_map(|i| {
                            let item = items.get(i)?;
                            Some(
                                set_row(&key, i, item, read_only, fg, muted_fg, border, cx)
                                    .into_any_element(),
                            )
                        })
                        .collect()
                }),
            )
            .track_scroll(scroll)
            .flex_1(),
        )
}

#[allow(clippy::too_many_arguments)]
fn set_row(
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
    let member_is_text = matches!(item, RedisValue::Text(_));
    let raw_member = match item {
        RedisValue::Text(text) if text.len() <= MAX_REDIS_COMMAND_ARG_BYTES => Some(text.clone()),
        _ => None,
    };
    let delete_disabled = read_only || raw_member.is_none();
    let key_for_emit = key.to_string();
    let del_id = SharedString::from(format!("set-del-{i}"));
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
            Button::new(del_id)
                .ghost()
                .small()
                .icon(ramag_ui::icons::trash())
                .disabled(delete_disabled)
                .tooltip(if read_only {
                    "生产连接为只读"
                } else if raw_member.is_none() {
                    if member_is_text {
                        "成员过大，请使用脚本处理"
                    } else {
                        "二进制成员暂不支持安全删除"
                    }
                } else {
                    "删除该成员"
                })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    if let Some(member) = raw_member.clone() {
                        cx.emit(KeyDetailEvent::RequestDeleteSetElement(
                            key_for_emit.clone(),
                            member,
                        ));
                    }
                })),
        )
}
