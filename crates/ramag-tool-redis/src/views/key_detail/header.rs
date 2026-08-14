//! Key 详情头部。

use gpui::{ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    clipboard::Clipboard, h_flex, v_flex,
};
use ramag_domain::entities::{MAX_REDIS_COLLECTION_BYTES, RedisValue};

use super::helpers::format_ttl_ms;
use super::{KeyDetailEvent, KeyDetailPanel, MAX_COLLECTION_ITEMS};
use crate::views::inline_text_preview;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_header(
    panel: &KeyDetailPanel,
    key: &str,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
) -> impl IntoElement + use<> {
    let ttl_label = match panel.ttl_ms {
        // PTTL：-1 为永久，-2 为 Key 不存在。
        Some(-1) => "永久".to_string(),
        Some(-2) => "Key 不存在".to_string(),
        Some(ms) if ms >= 0 => format_ttl_ms(ms),
        _ => "—".to_string(),
    };
    let key_for_ttl = key.to_string();
    let ttl_ms_for_event = panel.ttl_ms;
    let db = panel.db;
    // 仅借用值，避免渲染时复制大集合。
    let value_ref = panel.value.as_ref();
    let read_only = panel.is_read_only();

    let mut info_row = h_flex()
        .gap(px(10.0))
        .text_xs()
        .text_color(muted_fg)
        .child(div().child(format!("DB {db}")));

    // 类型标签。
    if let Some((label, color)) = value_ref.and_then(redis_type_label_color) {
        info_row = info_row.child(
            h_flex()
                .items_center()
                .gap(px(5.0))
                .child(
                    div()
                        .w(px(7.0))
                        .h(px(7.0))
                        .rounded_full()
                        .bg(color)
                        .flex_none(),
                )
                .child(div().child(label.to_string())),
        );
    }

    // TTL 失败时可单独重试。
    info_row = if panel.ttl_loading {
        info_row.child(
            ramag_ui::clickable_button("ttl-loading")
                .ghost()
                .xsmall()
                .label("TTL 获取中…")
                .disabled(true),
        )
    } else if let Some(error) = panel.ttl_error.as_ref() {
        info_row.child(
            ramag_ui::clickable_button("ttl-retry")
                .ghost()
                .xsmall()
                .label("重试")
                .text_color(gpui::red())
                .tooltip(error.clone())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload_ttl(cx))),
        )
    } else {
        info_row.child(
            ramag_ui::clickable_button("ttl-edit-trigger")
                .ghost()
                .xsmall()
                .label(format!("TTL {ttl_label} ✎"))
                .text_color(accent)
                .disabled(read_only)
                .when(read_only, |button| button.tooltip("只读"))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(KeyDetailEvent::RequestEditTtl(
                        key_for_ttl.clone(),
                        ttl_ms_for_event,
                    ));
                })),
        )
    };

    if let Some(loaded) = value_ref.and_then(RedisValue::len) {
        info_row = info_row.child(div().child(format!("{loaded} 元素")));
        if panel.value_memory_warning {
            info_row = info_row.child(
                div()
                    .text_color(accent)
                    .child("内存已超过 128 MiB，建议缩小加载范围"),
            );
        }
        if panel.has_more() && panel.value_byte_limited {
            info_row = info_row.child(div().text_color(muted_fg).child(format!(
                "内容已达到 {} MiB 安全上限",
                MAX_REDIS_COLLECTION_BYTES / 1024 / 1024
            )));
        } else if panel.has_more() {
            info_row = info_row.child(
                div()
                    .text_color(muted_fg)
                    .child(format!("已达到 {MAX_COLLECTION_ITEMS} 个安全上限")),
            );
        }
    } else if let Some(loaded) = value_ref.and_then(RedisValue::scalar_byte_len) {
        let label = match panel.collection_total {
            Some(total) if (loaded as u64) < total => {
                format!("已加载 {loaded} / {total} bytes")
            }
            _ => format!("{loaded} bytes"),
        };
        info_row = info_row.child(div().child(label));
    }

    info_row = info_row.child(render_size_chip(
        panel.key_size_bytes,
        panel.estimating_size,
        panel.size_error.as_deref(),
        muted_fg,
        accent,
        cx,
    ));

    let panel_for_copy = cx.entity();
    let copy_value_button = div()
        .debug_selector(|| "redis-value-copy-button".into())
        .child(
            Clipboard::new("redis-value-copy")
                .tooltip("复制")
                .value_fn(move |_, app| {
                    panel_for_copy
                        .read(app)
                        .value
                        .as_ref()
                        .map(RedisValue::to_clipboard_string)
                        .unwrap_or_default()
                        .into()
                })
                .on_copied(|_, window, cx| {
                    window.push_notification(ramag_ui::copy_success_notification(), cx);
                }),
        );

    let mut header = h_flex()
        .w_full()
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(border)
        .gap(px(12.0))
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(4.0))
                .child(
                    div()
                        .id("redis-key-title")
                        .debug_selector(|| "redis-key-title".into())
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .cursor_pointer()
                        .on_click({
                            let key = key.to_string();
                            move |event: &ClickEvent, window, cx| {
                                if ramag_ui::is_primary_modifier_double_click(event) {
                                    ramag_ui::copy_text_with_notification(key.clone(), window, cx);
                                }
                            }
                        })
                        .child(inline_text_preview(key, 256)),
                )
                .child(info_row),
        );

    header = header.child(copy_value_button);

    let key_owned = key.to_string();
    if let Some(value) = value_ref {
        let key_for_emit = key_owned.clone();
        header = match value {
            RedisValue::Hash(_) => header.child(add_btn(
                "redis-hash-add-field",
                "新增",
                read_only,
                cx,
                move || KeyDetailEvent::RequestAddHashField(key_for_emit.clone()),
            )),
            RedisValue::List(_) => header.child(add_btn(
                "redis-list-add-elem",
                "新增",
                read_only,
                cx,
                move || KeyDetailEvent::RequestAddListElement(key_for_emit.clone()),
            )),
            RedisValue::Set(_) => header.child(add_btn(
                "redis-set-add-elem",
                "新增",
                read_only,
                cx,
                move || KeyDetailEvent::RequestAddSetElement(key_for_emit.clone()),
            )),
            RedisValue::ZSet(_) => header.child(add_btn(
                "redis-zset-add-elem",
                "新增",
                read_only,
                cx,
                move || KeyDetailEvent::RequestAddZSetElement(key_for_emit.clone()),
            )),
            RedisValue::Stream(_) => header.child(add_btn(
                "redis-stream-add-entry",
                "新增",
                read_only,
                cx,
                move || KeyDetailEvent::RequestAddStreamEntry(key_for_emit.clone()),
            )),
            _ => header,
        };
    }

    let key_for_del = key_owned.clone();
    header.child(
        ramag_ui::clickable_button("redis-key-delete")
            .danger()
            .small()
            .icon(ramag_ui::icons::trash())
            .tooltip("删除")
            .when(read_only, |button| button.tooltip("只读"))
            .disabled(read_only)
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(KeyDetailEvent::RequestDeleteKey(key_for_del.clone()));
            })),
    )
}

fn add_btn<F>(
    id: &'static str,
    tooltip: &'static str,
    disabled: bool,
    cx: &mut Context<KeyDetailPanel>,
    make_event: F,
) -> impl IntoElement + use<F>
where
    F: Fn() -> KeyDetailEvent + 'static,
{
    ramag_ui::clickable_button(id)
        .outline()
        .small()
        .icon(IconName::Plus)
        .tooltip(if disabled { "只读" } else { tooltip })
        .disabled(disabled)
        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            cx.emit(make_event());
        }))
}

/// 显示或估算 Key 内存占用。
fn render_size_chip(
    bytes: Option<u64>,
    estimating: bool,
    error: Option<&str>,
    muted_fg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
) -> impl IntoElement + use<> {
    if let Some(n) = bytes {
        let label = format!("{}（{}）", human_readable_bytes(n), n);
        div()
            .id("size-result")
            .text_color(muted_fg)
            .child(label)
            .into_any_element()
    } else if estimating {
        div()
            .id("size-loading")
            .text_color(muted_fg)
            .child("估算中…")
            .into_any_element()
    } else if let Some(message) = error {
        ramag_ui::clickable_button("size-retry")
            .ghost()
            .xsmall()
            .label("重试")
            .text_color(gpui::red())
            .tooltip(message.to_string())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.estimate_size(cx)))
            .into_any_element()
    } else {
        ramag_ui::clickable_button("size-trigger")
            .ghost()
            .xsmall()
            .label("估算")
            .text_color(accent)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.estimate_size(cx)))
            .into_any_element()
    }
}

fn human_readable_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut idx = 0;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{n} B")
    } else {
        format!("{size:.2} {}", UNITS[idx])
    }
}

/// 从值类型推导标签和颜色。
pub(super) fn redis_type_label_color(v: &RedisValue) -> Option<(&'static str, gpui::Hsla)> {
    use gpui::hsla;
    match v {
        RedisValue::Text(_) | RedisValue::Bytes(_) => {
            Some(("String", hsla(210.0 / 360.0, 0.6, 0.55, 1.0)))
        }
        RedisValue::List(_) => Some(("List", hsla(140.0 / 360.0, 0.5, 0.5, 1.0))),
        RedisValue::Hash(_) => Some(("Hash", hsla(280.0 / 360.0, 0.55, 0.6, 1.0))),
        RedisValue::Set(_) => Some(("Set", hsla(40.0 / 360.0, 0.85, 0.55, 1.0))),
        RedisValue::ZSet(_) => Some(("ZSet", hsla(20.0 / 360.0, 0.7, 0.55, 1.0))),
        RedisValue::Stream(_) => Some(("Stream", hsla(330.0 / 360.0, 0.55, 0.55, 1.0))),
        _ => None,
    }
}
