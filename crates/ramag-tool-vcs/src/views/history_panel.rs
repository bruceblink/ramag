//! 提交历史、搜索和单文件过滤。

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement, Styled,
    div, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};

use ramag_domain::entities::Commit;

use super::commit_graph::CommitGraphRow;
use super::helpers::render_commit_row;
use super::sidebar::LeftRow;
use super::vcs_view::VcsView;

mod left_pane;

#[derive(Clone, Copy, PartialEq, Eq)]
struct HistoryLeftRowsCacheKey {
    local_identity: usize,
    local_len: usize,
    remote_identity: usize,
    remote_len: usize,
    tags_identity: usize,
    tags_len: usize,
    remotes_identity: usize,
    remotes_len: usize,
    collapsed_local: bool,
    collapsed_remote: bool,
    collapsed_tag: bool,
    collapsed_remote_repos: bool,
}

pub(super) struct HistoryLeftRowsCacheEntry {
    key: HistoryLeftRowsCacheKey,
    rows: Rc<Vec<LeftRow>>,
}

impl HistoryLeftRowsCacheEntry {
    fn get(&self, key: &HistoryLeftRowsCacheKey) -> Option<Rc<Vec<LeftRow>>> {
        (self.key == *key).then(|| self.rows.clone())
    }
}

impl VcsView {
    /// 渲染历史视图。
    pub(super) fn render_history_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let border = theme.border;
        let mono = theme.mono_font_family.clone();
        let busy = self.busy;

        let history_banner: AnyElement = if let Some(path) = &self.history_path_filter {
            let mut chip_bg = accent;
            chip_bg.a = 0.14;
            h_flex()
                .gap(px(8.0))
                .items_center()
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(chip_bg)
                .mb(px(8.0))
                .child(
                    Icon::new(ramag_ui::icons::scroll_text())
                        .small()
                        .text_color(accent),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(fg)
                        .font_family(mono.clone())
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(format!(
                            "正在看 {} 的历史",
                            super::inline_text_preview(path, 200)
                        )),
                )
                .child(
                    ramag_ui::clickable_button("vcs-history-clear-path")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("清除")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.clear_history_path_filter(cx);
                        })),
                )
                .into_any_element()
        } else if let Some(filter) = &self.history_ref_filter {
            let mut chip_bg = accent;
            chip_bg.a = 0.14;
            h_flex()
                .gap(px(8.0))
                .items_center()
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(chip_bg)
                .mb(px(8.0))
                .child(
                    Icon::new(ramag_ui::icons::git_branch())
                        .small()
                        .text_color(accent),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(fg)
                        .font_family(mono.clone())
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(format!(
                            "正在看 {} 的历史",
                            super::inline_text_preview(&filter.label, 200)
                        )),
                )
                .child(
                    ramag_ui::clickable_button("vcs-history-clear-ref")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("清除")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.clear_history_ref_filter(cx);
                        })),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let body: AnyElement =
            self.render_history_three_panel(border, fg, muted_fg, accent, mono, busy, cx);

        v_flex()
            .size_full()
            .pt(px(6.0))
            .pb(px(8.0))
            .gap(px(0.0))
            .child(div().px(px(12.0)).child(history_banner))
            .child(body)
            .into_any_element()
    }

    /// 渲染历史搜索栏。
    fn render_history_search_row(
        &self,
        busy: bool,
        muted_fg: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reflog_btn = ramag_ui::clickable_button("vcs-history-reflog-toggle")
            .ghost()
            .small()
            .icon(ramag_ui::icons::scroll_text())
            .tooltip(if self.showing_reflog {
                "提交历史"
            } else {
                "操作记录"
            })
            .disabled(busy)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_reflog(cx);
            }));
        h_flex()
            .gap(px(6.0))
            .items_center()
            .h(px(36.0))
            .flex_none()
            .px(px(8.0))
            .child(reflog_btn)
            .child(Icon::new(IconName::Search).small().text_color(muted_fg))
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(
                        &self.history_search_input,
                        "vcs-history-search-clear",
                        false,
                        cx,
                    )
                    .small()
                    .into_any_element(),
                ),
            )
            .when(!self.showing_reflog, |row| {
                // 操作记录在本地即时过滤，无需提交搜索。
                row.child(
                    ramag_ui::clickable_button("vcs-history-search")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
                        .tooltip("搜索")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.apply_history_search(cx);
                        })),
                )
            })
            .child(self.render_sync_quick_action(cx))
            .child(self.render_remote_actions(cx))
            .into_any_element()
    }

    /// 渲染分支、历史与详情三栏。
    #[allow(clippy::too_many_arguments)]
    fn render_history_three_panel(
        &self,
        border: gpui::Hsla,
        fg: gpui::Hsla,
        muted_fg: gpui::Hsla,
        accent: gpui::Hsla,
        mono: gpui::SharedString,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let left = self.render_history_left_pane(cx);
        let middle = if self.showing_reflog {
            self.render_reflog_middle_pane(busy, muted_fg, cx)
        } else {
            self.render_history_middle_pane(fg, muted_fg, accent, mono, busy, cx)
        };
        let show_detail = !self.showing_reflog && self.viewing_commit.is_some();

        let right_part: AnyElement = if show_detail {
            let detail = self.render_commit_detail_view(cx);
            gpui_component::resizable::h_resizable("vcs-history-detail-split")
                .with_state(&self.detail_resize)
                .child(
                    gpui_component::resizable::resizable_panel()
                        .child(div().size_full().min_w_0().child(middle)),
                )
                .child(
                    gpui_component::resizable::resizable_panel()
                        .size(px(280.0))
                        .size_range(px(220.0)..px(720.0))
                        .child(div().size_full().child(detail)),
                )
                .into_any_element()
        } else {
            div().size_full().min_w_0().child(middle).into_any_element()
        };

        gpui_component::resizable::h_resizable("vcs-history-bottom")
            .with_state(&self.ide_left_resize)
            .child(
                gpui_component::resizable::resizable_panel()
                    .size(px(super::ide_layout::LEFT_WIDTH_INITIAL))
                    .size_range(
                        px(super::ide_layout::LEFT_WIDTH_MIN)
                            ..px(super::ide_layout::LEFT_WIDTH_MAX),
                    )
                    .child(
                        div()
                            .id("vcs-history-left-column")
                            .debug_selector(|| "vcs-history-left-column".into())
                            .size_full()
                            .border_r_1()
                            .border_color(border)
                            .child(left),
                    ),
            )
            .child(
                gpui_component::resizable::resizable_panel()
                    .child(div().size_full().min_w_0().child(right_part)),
            )
            .into_any_element()
    }

    /// 渲染操作记录列表。
    fn render_reflog_middle_pane(
        &self,
        busy: bool,
        muted_fg: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .size_full()
            .min_h_0()
            .px(px(8.0))
            .child(self.render_history_search_row(busy, muted_fg, cx))
            .child(div().flex_1().min_h_0().child(self.render_reflog_view(cx)))
            .into_any_element()
    }

    /// 渲染提交历史列表。
    fn render_history_middle_pane(
        &self,
        fg: gpui::Hsla,
        muted_fg: gpui::Hsla,
        accent: gpui::Hsla,
        mono: gpui::SharedString,
        _busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let search_row = self.render_history_search_row(_busy, muted_fg, cx);
        if self.history_commits.is_empty() && self.loading_history {
            return v_flex()
                .size_full()
                .px(px(8.0))
                .child(search_row)
                .child(center_msg("加载中…", muted_fg))
                .into_any_element();
        }
        if self.history_commits.is_empty() {
            // 区分空仓库和无匹配结果。
            let filtered = !self.history_search_input.read(cx).value().trim().is_empty()
                || self.history_path_filter.is_some()
                || self.history_ref_filter.is_some();
            let hint = if filtered {
                "没有匹配的提交（调整搜索词或清除过滤）"
            } else {
                "（暂无提交记录）"
            };
            return v_flex()
                .size_full()
                .px(px(8.0))
                .child(search_row)
                .child(center_msg(hint, muted_fg))
                .into_any_element();
        }

        let count = self.history_commits.len();
        let has_more = self.history_has_more;
        let is_loading = self.loading_history;
        let total_rows = count + usize::from(has_more);
        // 共享列表，避免每帧复制。
        let commits_rc: Rc<Vec<Rc<Commit>>> = self.history_commits.clone();
        let graph_rc: Rc<Vec<CommitGraphRow>> = self.history_graph_rows.clone();

        let body = uniform_list(
            "vcs-history-commits",
            total_rows,
            cx.processor({
                let commits_rc = commits_rc.clone();
                let graph_rc = graph_rc.clone();
                let mono = mono.clone();
                move |this, range: Range<usize>, window, cx| {
                    let selected_id = this
                        .viewing_commit
                        .as_ref()
                        .map(|c| c.id.0.clone())
                        .unwrap_or_default();
                    range
                        .map(|i| {
                            if i == count && has_more {
                                if !is_loading {
                                    cx.defer_in(window, move |this, _, cx| {
                                        this.load_history_page(count, cx);
                                    });
                                }
                                return div()
                                    .h(px(28.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(if is_loading {
                                        "加载中…"
                                    } else {
                                        "加载更多…"
                                    })
                                    .into_any_element();
                            }
                            let is_selected = commits_rc[i].id.0 == selected_id;
                            div()
                                .h(px(28.0))
                                .flex_none()
                                .child(render_commit_row(
                                    &commits_rc[i],
                                    &graph_rc[i],
                                    mono.clone(),
                                    fg,
                                    muted_fg,
                                    accent,
                                    is_selected,
                                    cx,
                                ))
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.history_scroll)
        .flex_1();

        let footer: AnyElement = if self.history_limit_reached {
            div()
                .flex_none()
                .py(px(8.0))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(muted_fg)
                .child("已到历史上限，请用搜索或文件过滤缩小范围")
                .into_any_element()
        } else {
            div().flex_none().into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .px(px(8.0))
            .child(search_row)
            .child(body)
            .child(footer)
            .into_any_element()
    }
}

fn center_msg(msg: &'static str, muted_fg: gpui::Hsla) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(muted_fg)
        .child(msg)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_key(collapsed_local: bool) -> HistoryLeftRowsCacheKey {
        HistoryLeftRowsCacheKey {
            local_identity: 1,
            local_len: 2,
            remote_identity: 3,
            remote_len: 4,
            tags_identity: 5,
            tags_len: 6,
            remotes_identity: 7,
            remotes_len: 8,
            collapsed_local,
            collapsed_remote: true,
            collapsed_tag: true,
            collapsed_remote_repos: true,
        }
    }

    #[test]
    fn history_left_rows_cache_requires_exact_source_and_ui_state() {
        let key = cache_key(false);
        let rows = Rc::new(Vec::new());
        let entry = HistoryLeftRowsCacheEntry {
            key,
            rows: rows.clone(),
        };

        let cached = entry.get(&key);
        assert!(
            cached
                .as_ref()
                .is_some_and(|value| Rc::ptr_eq(value, &rows))
        );
        assert!(entry.get(&cache_key(true)).is_none());
    }
}
