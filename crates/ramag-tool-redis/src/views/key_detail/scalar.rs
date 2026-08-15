//! String 与 Bytes 值渲染。

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, ScrollWheelEvent, SharedString, Styled,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    Selectable as _, Sizable as _, WindowExt as _, button::ButtonVariants as _,
    clipboard::Clipboard, h_flex, v_flex,
};
use ramag_domain::entities::RedisValue;
use ramag_ui::RestrictScrollToAxisExt as _;

use super::{KeyDetailEvent, KeyDetailPanel};
use crate::views::value_display::{self, ViewMode};

/// 虚拟列表行高。
const ROW_H: f32 = 20.0;

use crate::views::value_display::{DISPLAY_CONTENT_WIDTH_PX, split_display_lines};

/// 生成标量值的显示内容。
fn compute_scalar_display(
    v: &RedisValue,
    view_mode: Option<ViewMode>,
    allow_gzip: bool,
) -> (
    ViewMode,
    SharedString,
    Arc<Vec<SharedString>>,
    Option<SharedString>,
) {
    let raw_bytes: &[u8] = match v {
        RedisValue::Text(s) => s.as_bytes(),
        RedisValue::Bytes(b) => b,
        _ => &[],
    };
    let (display_bytes, gzip_hint) = if allow_gzip {
        match value_display::try_decompress_gzip(raw_bytes) {
            Ok(Some(decoded)) => {
                let hint = format!(
                    "检测到 Gzip 压缩，已自动解压（原 {} bytes → {} bytes）",
                    raw_bytes.len(),
                    decoded.len()
                );
                (Cow::Owned(decoded), Some(hint.into()))
            }
            Ok(None) => (Cow::Borrowed(raw_bytes), None),
            Err(error) => (
                Cow::Borrowed(raw_bytes),
                Some(format!("检测到 Gzip，但未自动解压：{error}").into()),
            ),
        }
    } else {
        (Cow::Borrowed(raw_bytes), None)
    };
    let mode = view_mode.unwrap_or_else(|| value_display::auto_view_mode(&display_bytes));
    let content_text = match v {
        RedisValue::Text(_) => match std::str::from_utf8(&display_bytes) {
            Ok(s) => value_display::render_text(s, mode),
            Err(_) => value_display::render_bytes(&display_bytes, mode),
        },
        _ => value_display::render_bytes(&display_bytes, mode),
    };
    let display_text: SharedString = content_text.into();
    let lines = Arc::new(split_display_lines(display_text.as_str()));
    (mode, display_text, lines, gzip_hint)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_scalar(
    panel: &KeyDetailPanel,
    key: &str,
    v: &RedisValue,
    view_mode: Option<ViewMode>,
    scroll: &UniformListScrollHandle,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
    _window: &Window,
) -> impl IntoElement + use<> {
    let scalar_truncated = panel.scalar_is_truncated();
    // 同一视图模式复用缓存。
    let (mode, display_text, lines, gzip_hint) = {
        let mut cache = panel.scalar_cache.borrow_mut();
        if let Some((cached_req, eff_mode, display_text, lines, hint)) = cache.as_ref()
            && *cached_req == view_mode
        {
            (*eff_mode, display_text.clone(), lines.clone(), hint.clone())
        } else {
            let (eff_mode, display_text, lines, hint) =
                compute_scalar_display(v, view_mode, !scalar_truncated);
            *cache = Some((
                view_mode,
                eff_mode,
                display_text.clone(),
                lines.clone(),
                hint.clone(),
            ));
            (eff_mode, display_text, lines, hint)
        }
    };
    let line_count = lines.len();

    // 仅 Text 可双击编辑。
    let edit_target: Option<String> = match v {
        _ if panel.is_read_only() || scalar_truncated => None,
        RedisValue::Text(_) => Some(key.to_string()),
        _ => None,
    };

    // 虚拟化大值的可见行。
    let edit_target_for_click = edit_target.clone();
    let content_div = div()
        .id("redis-scalar-content")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .py(px(6.0))
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .on_click(cx.listener(move |panel, event: &ClickEvent, window, cx| {
            if ramag_ui::is_primary_modifier_double_click(event) {
                let text = panel
                    .scalar_cache
                    .borrow()
                    .as_ref()
                    .map(|(_, _, display_text, _, _)| display_text.clone())
                    .unwrap_or_default();
                ramag_ui::copy_text_with_notification(text.to_string(), window, cx);
                return;
            }
            if event.click_count() >= 2
                && let Some(key) = edit_target_for_click.clone()
                && let Some(RedisValue::Text(value)) = &panel.value
            {
                cx.emit(KeyDetailEvent::RequestEditValue(key, value.clone()));
            }
        }))
        .when_some(edit_target, |this, key| {
            let _ = key;
            this.cursor_pointer()
        })
        .child(
            // 输入层分流横纵滚动手势。
            div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("redis-scalar-hscroll")
                        .debug_selector(|| "redis-scalar-scroll-region".into())
                        .size_full()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&panel.scalar_h_scroll)
                        .child(
                            uniform_list(
                                "redis-scalar-lines",
                                line_count,
                                cx.processor(move |this, range: Range<usize>, _w, _cx| {
                                    let cache = this.scalar_cache.borrow();
                                    let Some((_, _, _, lines, _)) = cache.as_ref() else {
                                        return Vec::new();
                                    };
                                    range
                                        .filter_map(|index| lines.get(index).cloned())
                                        .map(|line| {
                                            div()
                                                .h(px(ROW_H))
                                                .px(px(10.0))
                                                .whitespace_nowrap()
                                                .text_sm()
                                                .text_color(fg)
                                                .font_family("monospace")
                                                .child(line)
                                                .into_any_element()
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(scroll)
                            .restrict_scroll_to_axis()
                            .h_full()
                            .w(px(DISPLAY_CONTENT_WIDTH_PX)),
                        ),
                )
                .child(
                    div()
                        .id("redis-scalar-scroll-input")
                        .absolute()
                        .inset_0()
                        .on_scroll_wheel(cx.listener(KeyDetailPanel::on_scalar_scroll)),
                ),
        );

    // 切换显示模式。
    let mode_row = h_flex()
        .gap(px(4.0))
        .children(
            [
                (ViewMode::Raw, "Raw"),
                (ViewMode::Json, "JSON"),
                (ViewMode::Hex, "Hex"),
                (ViewMode::Base64, "base64"),
            ]
            .into_iter()
            .map(|(m, label)| {
                ramag_ui::clickable_button(label)
                    .xsmall()
                    .ghost()
                    .selected(m == mode)
                    .label(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_value_view_mode(m, cx);
                    }))
            }),
        )
        .child(
            Clipboard::new("redis-scalar-copy")
                .tooltip("复制")
                .value(display_text)
                .on_copied(|_, window, cx| {
                    window.push_notification(ramag_ui::copy_success_notification(), cx);
                }),
        );

    v_flex()
        .size_full()
        .min_h_0()
        .gap(px(8.0))
        .child(mode_row)
        .when(scalar_truncated, |this| {
            let loaded = v.scalar_byte_len().unwrap_or_default();
            let total = panel.collection_total.unwrap_or(loaded as u64);
            this.child(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .border_1()
                    .border_color(border)
                    .rounded(px(4.0))
                    .child(format!(
                        "值过大，仅加载前 {loaded} / {total} bytes；已禁用编辑与 Gzip 自动解压，避免覆盖完整值"
                    )),
            )
        })
        .when_some(gzip_hint, |this, hint| {
            this.child(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_xs()
                    .text_color(muted_fg)
                    .border_1()
                    .border_color(border)
                    .rounded(px(4.0))
                    .child(hint),
            )
        })
        .child(content_div)
}

impl KeyDetailPanel {
    fn on_scalar_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let horizontal = self.scalar_h_scroll.clone();
        let vertical = self.value_scroll.0.borrow().base_handle.clone();
        ramag_ui::handle_axis_scroll(
            &mut self.scalar_scroll_gesture,
            event,
            window,
            &horizontal,
            &vertical,
            cx,
        );
    }
}
