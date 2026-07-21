//! 标量值（String / Bytes）渲染：视图模式切换（Raw/JSON/Hex/base64，按内容自动选默认）
//! + Gzip 提示 + 内容区（双击编辑，仅 Text）

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, UniformListScrollHandle,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{Selectable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};
use ramag_domain::entities::RedisValue;

use super::{KeyDetailEvent, KeyDetailPanel};
use crate::views::value_display::{self, ViewMode};

/// 等高行虚拟化的行高
const ROW_H: f32 = 20.0;

use crate::views::value_display::{DISPLAY_CONTENT_WIDTH_PX, split_display_lines};

/// 纯计算：字节流 → (生效 mode, 按行切好的显示文本, gzip 提示)。解压 + JSON 解析 +
/// pretty + 切行都在这里，结果由 panel.scalar_cache 缓存，避免每帧重算
fn compute_scalar_display(
    v: &RedisValue,
    view_mode: Option<ViewMode>,
    allow_gzip: bool,
) -> (ViewMode, Arc<Vec<SharedString>>, Option<SharedString>) {
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
    (
        mode,
        Arc::new(split_display_lines(&content_text)),
        gzip_hint,
    )
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
    // 缓存命中（同 view_mode）直接取；否则计算一次并写回缓存。
    // 缓存随 load_key / 切 view_mode 清空，此处仅比对 view_mode 兜底
    let (mode, lines, gzip_hint) = {
        let mut cache = panel.scalar_cache.borrow_mut();
        if let Some((cached_req, eff_mode, lines, hint)) = cache.as_ref()
            && *cached_req == view_mode
        {
            (*eff_mode, lines.clone(), hint.clone())
        } else {
            let (eff_mode, lines, hint) = compute_scalar_display(v, view_mode, !scalar_truncated);
            *cache = Some((view_mode, eff_mode, lines.clone(), hint.clone()));
            (eff_mode, lines, hint)
        }
    };
    let line_count = lines.len();

    // 编辑入口仅对 Text 类型开放（Bytes 二进制不支持文本编辑）：双击内容区打开编辑窗口
    let edit_target: Option<String> = match v {
        _ if panel.is_read_only() || scalar_truncated => None,
        RedisValue::Text(_) => Some(key.to_string()),
        _ => None,
    };

    // uniform_list 行级虚拟化：只渲染可见行，大值滚动不再整体排版
    let content_div = div()
        .id("redis-scalar-content")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .py(px(6.0))
        .border_1()
        .border_color(border)
        .rounded(px(4.0))
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
        .child(
            // 外层横向滚动（带 id 跨帧保位），内层固定内容宽，行尾不再被视口裁掉
            div()
                .id("redis-scalar-hscroll")
                .size_full()
                .overflow_x_scroll()
                .track_scroll(&panel.scalar_h_scroll)
                .child(
                    uniform_list(
                        "redis-scalar-lines",
                        line_count,
                        cx.processor(move |this, range: Range<usize>, _w, _cx| {
                            let cache = this.scalar_cache.borrow();
                            let Some((_, _, lines, _)) = cache.as_ref() else {
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
                    .h_full()
                    .w(px(DISPLAY_CONTENT_WIDTH_PX)),
                ),
        );

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
        .child(content_div)
}
