//! Split diff：左 gutter+content、中间列（回滚/blame）、右 gutter+content 共 5 个 uniform_list。
//! 分栏 diff 的滚动协调。
//! 五列共享纵向滚动，内容列独立横向滚动，gutter 和中间列保持可见。
//! `h_flex` 默认 items_center，必须显式 `.items_stretch()` 否则子栏会被压成内容高
mod middle;

use middle::*;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement,
    ScrollHandle, SharedString, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex,
};
use ramag_domain::entities::{DiffLineKind, FileDiff};
use ramag_ui::RestrictScrollToAxisExt as _;

use super::diff_keys::{SplitKey, UnifiedKey, build_split_keys, build_unified_keys};
use super::diff_panel::{
    CONTENT_PAD, DIFF_ROW_H, LINE_NO_W, MONO_CHAR_W, SPLIT_MARKER_W, render_diff_empty,
    render_diff_scroll_input, render_file_diff,
};
use super::diff_split_cells::{
    render_content_cell, render_content_header, render_content_spacer, render_gutter_cell,
    render_gutter_header, render_gutter_spacer,
};
use super::vcs_view::VcsView;

/// gutter 固定宽：marker(10) + lineno(40) = 50px
const SPLIT_GUTTER_W: f32 = SPLIT_MARKER_W + LINE_NO_W;

#[derive(Clone)]
pub(super) enum DiffLayout {
    Unified {
        keys: Rc<Vec<UnifiedKey>>,
        max_chars: usize,
    },
    Split {
        keys: Rc<Vec<SplitKey>>,
        button_rows: Rc<HashMap<usize, usize>>,
        max_chars: usize,
    },
}

pub(super) struct DiffLayoutCacheEntry {
    diff: Weak<FileDiff>,
    changes_only: bool,
    collapse: bool,
    expanded_spacers: HashSet<(usize, usize)>,
    layout: DiffLayout,
}

impl DiffLayoutCacheEntry {
    fn get(
        &self,
        diff: &Rc<FileDiff>,
        changes_only: bool,
        collapse: bool,
        expanded_spacers: &HashSet<(usize, usize)>,
    ) -> Option<DiffLayout> {
        let cached = self.diff.upgrade()?;
        (Rc::ptr_eq(&cached, diff)
            && self.changes_only == changes_only
            && self.collapse == collapse
            && self.expanded_spacers == *expanded_spacers)
            .then(|| self.layout.clone())
    }
}

/// 缓存 diff 扁平行、折叠布局与最大宽度；选择行、toast 等普通重渲染只做 O(1) Rc clone。
pub(super) fn prepare_diff_layout(
    cache: &RefCell<Option<DiffLayoutCacheEntry>>,
    diff: &Rc<FileDiff>,
    changes_only: bool,
    collapse: bool,
    expanded_spacers: &HashSet<(usize, usize)>,
) -> DiffLayout {
    if let Some(layout) = cache
        .borrow()
        .as_ref()
        .and_then(|entry| entry.get(diff, changes_only, collapse, expanded_spacers))
    {
        return layout;
    }

    let (has_old, has_new, max_old, max_new) = diff_metrics(diff, changes_only);
    let layout = if has_old && has_new {
        let keys = Rc::new(build_split_keys(
            diff,
            changes_only,
            collapse,
            expanded_spacers,
        ));
        let button_rows = Rc::new(hunk_button_rows(&keys));
        DiffLayout::Split {
            keys,
            button_rows,
            max_chars: max_old.max(max_new),
        }
    } else {
        DiffLayout::Unified {
            keys: Rc::new(build_unified_keys(diff, changes_only)),
            max_chars: max_old.max(max_new),
        }
    };
    cache.replace(Some(DiffLayoutCacheEntry {
        diff: Rc::downgrade(diff),
        changes_only,
        collapse,
        expanded_spacers: expanded_spacers.clone(),
        layout: layout.clone(),
    }));
    layout
}

fn diff_metrics(diff: &FileDiff, changes_only: bool) -> (bool, bool, usize, usize) {
    let mut has_old = false;
    let mut has_new = false;
    let mut max_old = 0usize;
    let mut max_new = 0usize;
    for h in &diff.hunks {
        for l in &h.lines {
            if changes_only && matches!(l.kind, DiffLineKind::Context) {
                continue;
            }
            let chars = super::syntax::display_cols(&l.text);
            match l.kind {
                DiffLineKind::Delete => {
                    has_old = true;
                    max_old = max_old.max(chars);
                }
                DiffLineKind::Add => {
                    has_new = true;
                    max_new = max_new.max(chars);
                }
                DiffLineKind::Context => {
                    has_old = true;
                    has_new = true;
                    max_old = max_old.max(chars);
                    max_new = max_new.max(chars);
                }
            }
        }
    }
    (has_old, has_new, max_old, max_new)
}

fn hunk_button_rows(keys: &[SplitKey]) -> HashMap<usize, usize> {
    let mut rows = HashMap::new();
    let mut index = 0;
    while index < keys.len() {
        if let SplitKey::Header { hunk_idx } = keys[index] {
            let mut end = index + 1;
            while end < keys.len() && !matches!(keys[end], SplitKey::Header { .. }) {
                end += 1;
            }
            let span = end - index;
            let middle = if span > 1 { index + span / 2 } else { index };
            rows.insert(middle, hunk_idx);
            index = end;
        } else {
            index += 1;
        }
    }
    rows
}

/// 渲染整个文件的 diff（Split 模式，IDEA 风格双栏独立横滚 + sticky gutter + 中间列）
#[allow(clippy::too_many_arguments)]
pub(super) fn render_file_diff_split(
    diff: &Rc<FileDiff>,
    syntax: Option<Rc<super::syntax::DiffSyntaxSnapshot>>,
    layout: DiffLayout,
    enable_discard: bool,
    mono: SharedString,
    _fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    muted_bg: gpui::Hsla,
    scroll: &UniformListScrollHandle,
    // 左右两栏共享同一横滚 handle，两栏一起横滚（IDEA 风格，避免错位无法对比）
    h_scroll: &ScrollHandle,
    has_blame: bool,
    allow_blame: bool,
    cx: &mut Context<VcsView>,
) -> AnyElement {
    if let Some(empty) = render_diff_empty(diff.as_ref(), muted_fg) {
        return empty;
    }
    let (keys, button_rows, max_chars) = match layout {
        DiffLayout::Unified { keys, max_chars } => {
            return render_file_diff(
                diff,
                keys,
                max_chars,
                syntax,
                mono,
                _fg,
                muted_fg,
                muted_bg,
                scroll,
                h_scroll,
                allow_blame,
                cx,
            );
        }
        DiffLayout::Split {
            keys,
            button_rows,
            max_chars,
        } => (keys, button_rows, max_chars),
    };

    // Rc clone：不复制 diff 本体（大 diff 每帧全量拷贝是主线程卡顿源）
    let diff_rc: Rc<FileDiff> = diff.clone();
    // 末尾追加一行空白，让最后一行代码不会紧贴面板底边。
    let total = keys.len().saturating_add(1);

    let scroll_v = scroll.clone();
    let h_shared = h_scroll.clone();
    let vertical_input = scroll.0.borrow().base_handle.clone();

    // 左右共用同一内容宽度（取较长侧）：共享横滚 handle 时两栏滚动范围才一致，都能滚到行尾
    let content_w = (max_chars as f32) * MONO_CHAR_W + CONTENT_PAD;
    // 中间列：仅回滚按钮时窄（28），需展示 blame author 时宽（96）
    // 中间列宽：blame 展示 140 / 未暂存两个按钮（暂存+丢弃）56 / 已暂存单按钮 28
    let middle_w = if has_blame { 140.0 } else { 56.0 };

    let left_gutter_list = build_gutter_list(
        "L",
        true,
        total,
        diff_rc.clone(),
        keys.clone(),
        mono.clone(),
        scroll_v.clone(),
        allow_blame,
        cx,
    );
    let left_content_list = build_content_list(
        "L",
        true,
        total,
        diff_rc.clone(),
        keys.clone(),
        syntax.clone(),
        mono.clone(),
        content_w,
        scroll_v.clone(),
        cx,
    );
    let middle_list = build_middle_list(
        total,
        diff_rc.clone(),
        keys.clone(),
        button_rows.clone(),
        enable_discard,
        has_blame,
        middle_w,
        scroll_v.clone(),
        cx,
    );
    let right_gutter_list = build_gutter_list(
        "R",
        false,
        total,
        diff_rc.clone(),
        keys.clone(),
        mono.clone(),
        scroll_v.clone(),
        allow_blame,
        cx,
    );
    let right_content_list = build_content_list(
        "R", false, total, diff_rc, keys, syntax, mono, content_w, scroll_v, cx,
    );

    div()
        .relative()
        .debug_selector(|| "vcs-diff-scroll-region".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .child(
            h_flex()
                .items_stretch()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(make_pane(
                    left_gutter_list,
                    left_content_list,
                    SPLIT_GUTTER_W,
                    content_w,
                    &h_shared,
                    "L",
                ))
                .child(div().flex_none().w(px(1.0)).h_full().bg(muted_fg))
                .child(
                    div()
                        .flex_none()
                        .w(px(middle_w))
                        .h_full()
                        .child(middle_list),
                )
                .child(div().flex_none().w(px(1.0)).h_full().bg(muted_fg))
                .child(make_pane(
                    right_gutter_list,
                    right_content_list,
                    SPLIT_GUTTER_W,
                    content_w,
                    &h_shared,
                    "R",
                )),
        )
        .child(render_diff_scroll_input(h_shared, vertical_input, cx))
        .into_any_element()
}

/// 单栏布局：[gutter 固定 w][content overflow_x_scroll]
fn make_pane(
    gutter: gpui::UniformList,
    content: gpui::UniformList,
    gutter_w: f32,
    content_w: f32,
    h_handle: &ScrollHandle,
    side: &'static str,
) -> impl IntoElement {
    h_flex()
        .items_stretch()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .child(div().flex_none().w(px(gutter_w)).h_full().child(gutter))
        .child(
            div()
                .id(SharedString::from(format!("vcs-diff-{side}-h-scroll")))
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(h_handle)
                .child(
                    gpui_component::v_flex()
                        .min_w_full()
                        .w(px(content_w))
                        .h_full()
                        .child(content),
                ),
        )
}

/// 构建一栏的 gutter uniform_list（钉死区）
#[allow(clippy::too_many_arguments)]
fn build_gutter_list(
    side: &'static str,
    is_left: bool,
    total: usize,
    diff_rc: Rc<FileDiff>,
    keys: Rc<Vec<SplitKey>>,
    mono: SharedString,
    scroll_v: UniformListScrollHandle,
    allow_blame: bool,
    cx: &mut Context<VcsView>,
) -> gpui::UniformList {
    uniform_list(
        SharedString::from(format!("vcs-diff-{side}-gutter")),
        total,
        cx.processor(move |_this, range: Range<usize>, _w, cx| {
            let theme = cx.theme();
            let muted_fg = theme.muted_foreground;
            let muted_bg = theme.muted;
            range
                .map(|i| {
                    if i == keys.len() {
                        return div().w_full().h(px(DIFF_ROW_H)).into_any_element();
                    }
                    match keys[i] {
                        SplitKey::Header { .. } => render_gutter_header(muted_bg),
                        SplitKey::Pair {
                            hunk_idx,
                            left,
                            right,
                        } => {
                            let line_idx = if is_left { left } else { right };
                            let line = line_idx.map(|li| (li, &diff_rc.hunks[hunk_idx].lines[li]));
                            render_gutter_cell(
                                side,
                                line,
                                hunk_idx,
                                is_left,
                                muted_fg,
                                mono.clone(),
                                allow_blame,
                                cx,
                            )
                        }
                        SplitKey::Spacer { .. } => render_gutter_spacer(side, muted_bg),
                    }
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&scroll_v)
    .h_full()
    .min_h_0()
}

/// 构建一栏的 content uniform_list（横滚区，仅渲染代码文本）
#[allow(clippy::too_many_arguments)]
fn build_content_list(
    side: &'static str,
    is_left: bool,
    total: usize,
    diff_rc: Rc<FileDiff>,
    keys: Rc<Vec<SplitKey>>,
    syntax: Option<Rc<super::syntax::DiffSyntaxSnapshot>>,
    mono: SharedString,
    content_w: f32,
    scroll_v: UniformListScrollHandle,
    cx: &mut Context<VcsView>,
) -> gpui::UniformList {
    uniform_list(
        SharedString::from(format!("vcs-diff-{side}-content")),
        total,
        cx.processor(move |_this, range: Range<usize>, _w, cx| {
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
                        SplitKey::Header { hunk_idx } => render_content_header(
                            &diff_rc.hunks[hunk_idx],
                            mono.clone(),
                            muted_fg,
                            muted_bg,
                        ),
                        SplitKey::Pair {
                            hunk_idx,
                            left,
                            right,
                        } => {
                            let line_idx = if is_left { left } else { right };
                            let line = line_idx.map(|li| (li, &diff_rc.hunks[hunk_idx].lines[li]));
                            let code_line = line.map(|(line_idx, line)| {
                                syntax
                                    .as_ref()
                                    .and_then(|syntax| {
                                        syntax.side_line(
                                            hunk_idx,
                                            line_idx,
                                            is_left,
                                            &highlight_theme,
                                            theme_key,
                                        )
                                    })
                                    .unwrap_or_else(|| super::syntax::plain_code_line(&line.text))
                            });
                            render_content_cell(
                                side,
                                line,
                                hunk_idx,
                                code_line,
                                fg,
                                mono.clone(),
                                content_w,
                                cx,
                            )
                        }
                        SplitKey::Spacer {
                            hunk_idx,
                            run_start,
                            skipped,
                        } => {
                            render_content_spacer(side, hunk_idx, run_start, skipped, muted_fg, cx)
                        }
                    }
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&scroll_v)
    .w(px(content_w))
    .min_w_full()
    .restrict_scroll_to_axis()
    .h_full()
    .min_h_0()
}

#[cfg(test)]
mod tests;
