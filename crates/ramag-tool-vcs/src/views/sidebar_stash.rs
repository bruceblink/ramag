//! Stash 列表渲染（IDE Files panel 的 Stash 视图主区调用）
//!
//! 行尾按钮 [Apply][Pop][Drop]，每条 stash 显示 stash@{N} + message + 时间。

use std::ops::Range;
use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px, uniform_list};
use gpui_component::{ActiveTheme, IconName, h_flex, v_flex};
use ramag_domain::entities::{Stash, contains_case_insensitive};

use super::helpers::{StashOp, side_op_button};
use super::vcs_view::VcsView;

impl VcsView {
    /// Stash 列表 body：供 IDE Files panel Stash 视图主区调用
    pub(super) fn render_stash_list_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted_fg = cx.theme().muted_foreground;
        let busy = self.busy
            || self
                .status
                .as_ref()
                .and_then(|status| status.operation)
                .is_some();
        if self.loading_stashes {
            return div()
                .pl(px(4.0))
                .text_xs()
                .text_color(muted_fg)
                .child("加载中…")
                .into_any_element();
        }
        if self.stashes.is_empty() {
            return div()
                .pl(px(4.0))
                .text_xs()
                .text_color(muted_fg)
                .child("暂无 stash（工具栏「Stash 工作区改动」创建）")
                .into_any_element();
        }
        // Files panel 搜索框在 Stash 模式按描述过滤（与 Changes/Project 的路径过滤语义对齐）
        let search = self.files_search_input.read(cx);
        let search_value = search.value();
        let query = search_value.trim();
        let filtered_indices = (!query.is_empty()).then(|| {
            Rc::new(
                self.stashes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, stash)| {
                        contains_case_insensitive(&stash.message, query).then_some(index)
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let row_count = filtered_indices
            .as_ref()
            .map_or(self.stashes.len(), |indices| indices.len());
        if row_count == 0 {
            return div()
                .pl(px(4.0))
                .text_xs()
                .text_color(muted_fg)
                .child("没有匹配的 stash")
                .into_any_element();
        }
        uniform_list(
            "vcs-stash-rows",
            row_count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|visible_index| {
                        let source_index = filtered_indices
                            .as_ref()
                            .and_then(|indices| indices.get(visible_index).copied())
                            .unwrap_or(visible_index);
                        this.stashes
                            .get(source_index)
                            .map(|stash| stash_row(stash, busy, cx).into_any_element())
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.stash_scroll)
        .flex_1()
        .min_h_0()
        .gap(px(2.0))
        .into_any_element()
    }
}

/// 单条 stash 行：紧凑布局 stash@{N} + msg + 行尾按钮
fn stash_row(s: &Stash, busy: bool, cx: &mut Context<VcsView>) -> impl IntoElement {
    let theme = cx.theme();
    let fg = theme.foreground;
    let muted_fg = theme.muted_foreground;
    let mono = theme.mono_font_family.clone();
    let idx = s.id.0;

    v_flex()
        .gap(px(2.0))
        .py(px(3.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .child(
            h_flex()
                .gap(px(6.0))
                .items_baseline()
                .child(
                    div()
                        .flex_none()
                        .font_family(mono)
                        .text_xs()
                        .text_color(theme.accent)
                        .child(format!("stash@{{{idx}}}")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(super::inline_text_preview(&s.message, 240)),
                ),
        )
        .child(
            h_flex()
                .gap(px(6.0))
                .items_center()
                .child(side_op_button(
                    format!("vcs-side-stash-apply-{idx}"),
                    "应用（保留 stash）",
                    IconName::ArrowDown,
                    busy,
                    move |this, window, cx| this.confirm_stash_op(StashOp::Apply(idx), window, cx),
                    cx,
                ))
                .child(side_op_button(
                    format!("vcs-side-stash-pop-{idx}"),
                    "应用并删除 stash",
                    IconName::Check,
                    busy,
                    move |this, window, cx| this.confirm_stash_op(StashOp::Pop(idx), window, cx),
                    cx,
                ))
                .child(side_op_button(
                    format!("vcs-side-stash-drop-{idx}"),
                    "丢弃 stash",
                    ramag_ui::icons::trash(),
                    busy,
                    move |this, window, cx| this.confirm_stash_op(StashOp::Drop(idx), window, cx),
                    cx,
                ))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(s.timestamp.format("%m-%d %H:%M").to_string()),
                ),
        )
}
