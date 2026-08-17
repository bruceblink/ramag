//! Diff 面板：Unified（行号双列 + `+/-`）/ Split（左旧右新对齐）。点行号 = inline blame

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement,
    ScrollHandle, SharedString, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use gpui_component::{ActiveTheme, h_flex};
use ramag_domain::entities::{DiffLine, DiffLineKind, FileDiff};
use ramag_ui::RestrictScrollToAxisExt as _;

use super::vcs_view::VcsView;

/// 单行高度（uniform_list 要求等高，hunk header 也压缩到这个高度）
pub(super) const DIFF_ROW_H: f32 = 20.0;
/// 等宽字体单字符估算宽度（mono 13px size 下约 7.5px/字，与 pf_content 同款）
pub(super) const MONO_CHAR_W: f32 = 7.5;
pub(super) const LINE_NO_W: f32 = 40.0;
const UNIFIED_MARKER_W: f32 = 14.0;
pub(super) const SPLIT_MARKER_W: f32 = 10.0;
pub(super) const CONTENT_PAD: f32 = 8.0;

use super::diff_keys::UnifiedKey;

/// Unified diff。固定 list w + 外层 overflow_x_scroll 共享 ScrollHandle，restrict_scroll_to_axis 防 wheel 错位
#[allow(clippy::too_many_arguments)]
pub(super) fn render_file_diff(
    diff: &Rc<FileDiff>,
    keys: Rc<Vec<UnifiedKey>>,
    max_chars: usize,
    syntax: Option<Rc<super::syntax::DiffSyntaxSnapshot>>,
    mono: SharedString,
    _fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    _muted_bg: gpui::Hsla,
    scroll: &UniformListScrollHandle,
    h_scroll: &ScrollHandle,
    allow_blame: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    if let Some(empty) = render_diff_empty(diff.as_ref(), muted_fg) {
        return empty;
    }
    // Rc clone：不复制 diff 本体（大 diff 每帧全量拷贝是主线程卡顿源）
    let diff_rc: Rc<FileDiff> = diff.clone();
    let total = keys.len().saturating_add(1);
    let scroll = scroll.clone();
    let h_scroll = h_scroll.clone();

    let content_w = (max_chars as f32) * MONO_CHAR_W + CONTENT_PAD;
    let total_w = LINE_NO_W * 2.0 + UNIFIED_MARKER_W + content_w;

    let body = uniform_list(
        "vcs-diff-unified",
        total,
        cx.processor({
            let diff_rc = diff_rc.clone();
            let keys = keys.clone();
            let mono = mono.clone();
            move |_this, range: Range<usize>, _w, cx| {
                let theme = cx.theme();
                let fg = theme.foreground;
                let muted_fg = theme.muted_foreground;
                let muted_bg = theme.muted;
                let highlight_theme = theme.highlight_theme.clone();
                let theme_key = super::syntax::highlight_theme_key(&highlight_theme);
                range
                    .map(|i| {
                        if i == keys.len() {
                            return div().w_full().h(px(DIFF_ROW_H)).into_any_element();
                        }
                        match keys[i] {
                            UnifiedKey::Header { hunk_idx } => render_hunk_header_unified(
                                &diff_rc.hunks[hunk_idx],
                                hunk_idx,
                                false,
                                mono.clone(),
                                muted_fg,
                                muted_bg,
                                cx,
                            )
                            .into_any_element(),
                            UnifiedKey::Line { hunk_idx, line_idx } => {
                                let line = &diff_rc.hunks[hunk_idx].lines[line_idx];
                                let code_line = syntax
                                    .as_ref()
                                    .and_then(|syntax| {
                                        syntax.unified_line(
                                            hunk_idx,
                                            line_idx,
                                            line.kind,
                                            &highlight_theme,
                                            theme_key,
                                        )
                                    })
                                    .unwrap_or_else(|| super::syntax::plain_code_line(&line.text));
                                render_diff_line(
                                    line,
                                    hunk_idx,
                                    line_idx,
                                    code_line,
                                    mono.clone(),
                                    fg,
                                    muted_fg,
                                    content_w,
                                    allow_blame,
                                    cx,
                                )
                                .into_any_element()
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            }
        }),
    )
    .track_scroll(&scroll)
    .w(px(total_w))
    .min_w_full()
    .restrict_scroll_to_axis()
    .flex_1();

    let vertical_input = scroll.0.borrow().base_handle.clone();
    div()
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .child(
            div()
                .id("vcs-diff-unified-h-scroll")
                .debug_selector(|| "vcs-diff-scroll-region".into())
                .size_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&h_scroll)
                .child(
                    gpui_component::v_flex()
                        .min_w_full()
                        .w(px(total_w))
                        .h_full()
                        .child(body),
                ),
        )
        .child(render_diff_scroll_input(h_scroll, vertical_input, cx))
        .into_any_element()
}

pub(super) fn render_diff_scroll_input(
    horizontal: ScrollHandle,
    vertical: ScrollHandle,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    div()
        .id("vcs-diff-scroll-input")
        .absolute()
        .inset_0()
        .on_scroll_wheel(cx.listener(move |this, event, window, cx| {
            ramag_ui::handle_axis_scroll(
                &mut this.diff_scroll_gesture,
                event,
                window,
                &horizontal,
                &vertical,
                cx,
            );
        }))
}

pub(super) fn render_hunk_header_unified(
    hunk: &ramag_domain::entities::Hunk,
    hunk_idx: usize,
    enable_discard: bool,
    mono: SharedString,
    muted_fg: gpui::Hsla,
    muted_bg: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    render_hunk_header_common(hunk, hunk_idx, enable_discard, mono, muted_fg, muted_bg, cx)
}

pub(super) fn render_hunk_header_common(
    hunk: &ramag_domain::entities::Hunk,
    hunk_idx: usize,
    enable_discard: bool,
    mono: SharedString,
    muted_fg: gpui::Hsla,
    muted_bg: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let header_text = format!(
        "@@ -{},{} +{},{} @@{}",
        hunk.old_start,
        hunk.old_lines,
        hunk.new_start,
        hunk.new_lines,
        match &hunk.heading {
            Some(h) => format!(" {h}"),
            None => String::new(),
        }
    );
    let row = h_flex()
        .w_full()
        .h(px(DIFF_ROW_H))
        .flex_none()
        .px(px(8.0))
        .bg(muted_bg)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(muted_fg)
        .font_family(mono)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(header_text),
        );
    let _ = enable_discard;
    let _ = hunk_idx;
    let _ = cx;
    row.into_any_element()
}

pub(super) fn render_diff_empty(diff: &FileDiff, muted_fg: gpui::Hsla) -> Option<AnyElement> {
    if diff.binary {
        return Some(
            div()
                .px(px(12.0))
                .py(px(20.0))
                .text_sm()
                .text_color(muted_fg)
                .child("（二进制文件，不渲染内容）")
                .into_any_element(),
        );
    }
    if diff.hunks.is_empty() {
        return Some(
            div()
                .px(px(12.0))
                .py(px(20.0))
                .text_sm()
                .text_color(muted_fg)
                .child("（无差异）")
                .into_any_element(),
        );
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn render_diff_line(
    line: &DiffLine,
    hunk_idx: usize,
    line_idx: usize,
    code_line: super::syntax::CodeLine,
    mono: SharedString,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    content_w: f32,
    allow_blame: bool,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    let (bg, marker, marker_color) = line_palette(line.kind);
    let row_id = SharedString::from(format!("vcs-diff-line-{hunk_idx}-{line_idx}"));
    let old_id = SharedString::from(format!("vcs-diff-old-{hunk_idx}-{line_idx}"));
    let new_id = SharedString::from(format!("vcs-diff-new-{hunk_idx}-{line_idx}"));
    let content_id = SharedString::from(format!("vcs-diff-content-{hunk_idx}-{line_idx}"));
    let line_for_copy = line.text.clone();
    let content = div()
        .id(content_id)
        .flex_1()
        .min_w(px(content_w))
        .px(px(4.0))
        .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
            if ramag_ui::is_primary_modifier_double_click(event) {
                ramag_ui::copy_text(line_for_copy.clone(), cx);
            }
        }))
        .child(super::syntax::render_code_line(code_line, fg, mono.clone()));
    let mut row = h_flex()
        .id(row_id)
        .w_full()
        .h(px(DIFF_ROW_H))
        .flex_none()
        .gap(px(0.0))
        .font_family(mono.clone())
        .text_xs()
        .child(line_no_cell_clickable(
            line.old_lineno,
            true,
            old_id,
            muted_fg,
            false,
            cx,
        ))
        .child(line_no_cell_clickable(
            line.new_lineno,
            false,
            new_id,
            muted_fg,
            allow_blame,
            cx,
        ))
        .child(
            div()
                .flex_none()
                .w(px(UNIFIED_MARKER_W))
                .text_color(marker_color)
                .child(marker),
        )
        .child(content);

    if let Some(c) = bg {
        row = row.bg(c);
    }
    row
}

pub(super) fn line_no_cell(label: String, muted_fg: gpui::Hsla) -> impl IntoElement {
    h_flex()
        .flex_none()
        .w(px(40.0))
        .px(px(4.0))
        .justify_end()
        .text_color(muted_fg)
        .child(label)
}

pub(super) fn line_no_cell_clickable(
    line_no: Option<u32>,
    is_old: bool,
    cell_id: SharedString,
    muted_fg: gpui::Hsla,
    enabled: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    let label = line_no.map(|n| n.to_string()).unwrap_or_default();
    let mut cell = h_flex()
        .id(cell_id)
        .flex_none()
        .w(px(40.0))
        .px(px(4.0))
        .justify_end()
        .text_color(muted_fg)
        .child(label);
    if enabled && let Some(n) = line_no {
        cell = cell
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.show_inline_blame(n, is_old, cx);
            }));
    }
    cell.into_any_element()
}

pub(super) fn line_palette(kind: DiffLineKind) -> (Option<gpui::Hsla>, &'static str, gpui::Hsla) {
    match kind {
        DiffLineKind::Context => (None, " ", gpui::hsla(0.0, 0.0, 0.5, 1.0)),
        DiffLineKind::Add => (
            Some(gpui::hsla(140.0 / 360.0, 0.55, 0.85, 0.30)),
            "+",
            gpui::hsla(140.0 / 360.0, 0.55, 0.40, 1.0),
        ),
        DiffLineKind::Delete => (
            Some(gpui::hsla(0.0, 0.65, 0.85, 0.30)),
            "-",
            gpui::hsla(0.0, 0.65, 0.50, 1.0),
        ),
    }
}
