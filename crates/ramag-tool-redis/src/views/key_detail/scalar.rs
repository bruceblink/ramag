//! 标量值（String / Bytes）渲染：视图模式切换（Raw/JSON/Hex/base64，按内容自动选默认）
//! + Gzip 提示 + 内容区（双击编辑，仅 Text）

use std::borrow::Cow;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*,
    px,
};
use gpui_component::{Selectable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use ramag_domain::entities::RedisValue;

use super::{KeyDetailEvent, KeyDetailPanel};
use crate::views::value_display::{self, ViewMode};

/// 纯计算：字节流 → (生效 mode, 显示文本, gzip 提示)。解压 + JSON 解析 + pretty 都在
/// 这里，结果由 panel.scalar_cache 缓存，避免每帧重算
fn compute_scalar_display(
    v: &RedisValue,
    view_mode: Option<ViewMode>,
    allow_gzip: bool,
) -> (ViewMode, SharedString, Option<SharedString>) {
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
    (mode, content_text.into(), gzip_hint)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_scalar(
    panel: &KeyDetailPanel,
    key: &str,
    v: &RedisValue,
    view_mode: Option<ViewMode>,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    border: gpui::Hsla,
    cx: &mut Context<KeyDetailPanel>,
    _window: &Window,
) -> impl IntoElement + use<> {
    let scalar_truncated = panel.scalar_is_truncated();
    // 缓存命中（同 view_mode）直接取；否则计算一次并写回缓存。
    // 缓存随 load_key / 切 view_mode 清空，此处仅比对 view_mode 兜底
    let (mode, content_text, gzip_hint) = {
        let mut cache = panel.scalar_cache.borrow_mut();
        if let Some((cached_req, eff_mode, text, hint)) = cache.as_ref()
            && *cached_req == view_mode
        {
            (*eff_mode, text.clone(), hint.clone())
        } else {
            let (eff_mode, text, hint) = compute_scalar_display(v, view_mode, !scalar_truncated);
            *cache = Some((view_mode, eff_mode, text.clone(), hint.clone()));
            (eff_mode, text, hint)
        }
    };

    // 编辑入口仅对 Text 类型开放（Bytes 二进制不支持文本编辑）：双击内容区打开编辑窗口
    let edit_target: Option<String> = match v {
        _ if panel.is_read_only() || scalar_truncated => None,
        RedisValue::Text(_) => Some(key.to_string()),
        _ => None,
    };

    let content_div = div()
        .id("redis-scalar-content")
        .flex_1()
        .min_w_0()
        .p(px(10.0))
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
        .text_sm()
        .text_color(fg)
        .font_family("monospace")
        .when_some(edit_target, |this, key| {
            this.cursor_pointer()
                .on_click(cx.listener(move |panel, e: &ClickEvent, _, cx| {
                    if e.click_count() >= 2
                        && let Some(RedisValue::Text(value)) = &panel.value
                    {
                        cx.emit(KeyDetailEvent::RequestEditValue(key.clone(), value.clone()));
                    }
                }))
        })
        .child(content_text);

    let content_row = h_flex().w_full().child(content_div);

    // 视图模式切换：Raw / JSON / Hex / base64，高亮当前生效模式；点击即固定为手动模式
    let mode_row = h_flex().gap(px(4.0)).children(
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
    );

    v_flex()
        .w_full()
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
        // Gzip 自动解压提示
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
        .child(content_row)
}
