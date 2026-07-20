//! Hash 块：uniform_list 行级虚拟化（等高行），双击行编辑字段 + 删除按钮

use std::ops::Range;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, UniformListScrollHandle,
    div, prelude::*, px, uniform_list,
};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex};
use ramag_domain::entities::{MAX_REDIS_COMMAND_ARG_BYTES, RedisValue};

use super::{KeyDetailEvent, KeyDetailPanel};
use crate::views::inline_text_preview;

/// 行高固定 32px：uniform_list 行级虚拟化要求等高
const ROW_H: f32 = 32.0;

pub(super) fn render_hash_block(
    panel: &mut Context<KeyDetailPanel>,
    key: String,
    count: usize,
    scroll: &UniformListScrollHandle,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement + use<> {
    div()
        .debug_selector(|| "redis-hash-block".into())
        .flex_col()
        .size_full()
        .min_h_0()
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .child(
            uniform_list(
                "hash-rows",
                count,
                panel.processor(move |this, range: Range<usize>, _w, cx| {
                    let Some(RedisValue::Hash(pairs)) = &this.value else {
                        return Vec::new();
                    };
                    let read_only = this.is_read_only();
                    range
                        .filter_map(|idx| {
                            let (f, v) = pairs.get(idx)?;
                            Some(
                                hash_row(&key, idx, f, v, read_only, fg, muted_fg, border, cx)
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
fn hash_row(
    key: &str,
    idx: usize,
    field: &str,
    value: &RedisValue,
    read_only: bool,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
) -> impl IntoElement + use<> {
    let field_preview = inline_text_preview(field, 128);
    let value_preview = value.display_preview(256);
    // HSCAN 的字段名目前以 UTF-8 字符串展示；出现替换字符表明原始字节已无法
    // 安全往返，禁用编辑/删除以避免将损坏后的文本当作真实字段名。
    let field_is_lossy = field.contains('\u{fffd}');
    // 仅文本值可编辑：二进制值显示的是 `[N bytes]` 摘要，若允许编辑会把真实二进制
    // 覆盖成摘要串（静默毁数据），故非文本值只读（双击不打开编辑窗）
    let field_for_delete =
        (!field_is_lossy && field.len() <= MAX_REDIS_COMMAND_ARG_BYTES).then(|| field.to_string());
    let field_for_edit = field_for_delete
        .as_ref()
        .filter(|field| !field.chars().any(char::is_control))
        .cloned();
    let value_for_edit = match value {
        RedisValue::Text(text) if text.len() <= MAX_REDIS_COMMAND_ARG_BYTES => Some(text.clone()),
        _ => None,
    };
    let editable = !read_only && field_for_edit.is_some() && value_for_edit.is_some();
    let delete_disabled = read_only || field_for_delete.is_none();
    let key_for_edit = key.to_string();
    let value_for_edit_clone = value_for_edit;
    let key_for_del = key.to_string();
    let field_for_del = field_for_delete;
    let row_id = SharedString::from(format!("hash-row-{idx}"));
    let del_id = SharedString::from(format!("hash-del-{idx}"));

    h_flex()
        .id(row_id)
        .h(px(ROW_H))
        .flex_none()
        .w_full()
        .px(px(8.0))
        .border_b_1()
        .border_color(border)
        .gap(px(8.0))
        .items_center()
        .when(editable, |row| row.cursor_pointer())
        // 双击该行打开编辑窗口（仅文本值可编辑，二进制只读）
        .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
            if editable
                && e.click_count() >= 2
                && let (Some(field), Some(value)) =
                    (field_for_edit.clone(), value_for_edit_clone.clone())
            {
                cx.emit(KeyDetailEvent::RequestEditHashField(
                    key_for_edit.clone(),
                    field,
                    value,
                ));
            }
        }))
        .child(
            div()
                .w(px(160.0))
                .text_xs()
                .text_color(muted_fg)
                .flex_none()
                .overflow_hidden()
                .text_ellipsis()
                .child(field_preview),
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
                .child(value_preview),
        )
        .child(
            ramag_ui::clickable_button(del_id)
                .ghost()
                .small()
                .icon(ramag_ui::icons::trash())
                .disabled(delete_disabled)
                .tooltip(if read_only {
                    "生产连接为只读"
                } else if field_is_lossy {
                    "二进制字段名暂不支持安全删除"
                } else if field.len() > MAX_REDIS_COMMAND_ARG_BYTES {
                    "字段名过大，请使用脚本处理"
                } else {
                    "删除该字段"
                })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    if let Some(field) = field_for_del.clone() {
                        cx.emit(KeyDetailEvent::RequestDeleteHashField(
                            key_for_del.clone(),
                            field,
                        ));
                    }
                })),
        )
}
