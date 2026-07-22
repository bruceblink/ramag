//! History：commit 列表分页 + 搜索（关键词 / `@作者` / `7d`/`1m`）+ 单文件历史 banner。
//! viewing_commit.is_some() 时整区切到 commit_detail

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement, Styled,
    div, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, input::Input, v_flex,
};

use ramag_domain::entities::Commit;

use super::commit_graph::CommitGraphRow;
use super::helpers::render_commit_row;
use super::sidebar::{LeftRow, SidebarSection};
use super::vcs_view::VcsView;

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
    /// 历史视图：commit list / 详情视图（点击 commit 行后）/ reflog（搜索行按钮 toggle 后）
    pub(super) fn render_history_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let border = theme.border;
        let mono = theme.mono_font_family.clone();
        let busy = self.busy;

        let path_banner: AnyElement = if let Some(path) = &self.history_path_filter {
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
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.clear_history_path_filter(cx);
                        })),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        // 统一走三栏布局：reflog 与 commit 模式均保留左栏分支视图
        // 中栏内容由 showing_reflog 切换；右栏 commit 详情仅 commit 模式可见
        let body: AnyElement =
            self.render_history_three_panel(border, fg, muted_fg, accent, mono, busy, cx);

        v_flex()
            .size_full()
            .px(px(12.0))
            .pt(px(6.0))
            .pb(px(8.0))
            .gap(px(0.0))
            .child(path_banner)
            .child(body)
            .into_any_element()
    }

    /// commit / reflog 列表共用搜索行
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
                "提交"
            } else {
                "Reflog"
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
                    Input::new(&self.history_search_input)
                        .small()
                        .into_any_element(),
                ),
            )
            .when(!self.showing_reflog, |row| {
                // commit 模式：搜索走 git（grep/author/since），需显式应用；
                // reflog 模式为客户端即时过滤，无需应用按钮
                row.child(
                    ramag_ui::clickable_button("vcs-history-search")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
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

    /// 双栏：左分支 / 右半（含 commit graph + 内部 detail resizable）。
    /// 外层永远 2 children 与上半共用 `ide_left_resize` 同步对齐；reflog 模式右栏 detail 隐藏
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
        let left = self.render_history_left_pane(border, cx);
        let middle = if self.showing_reflog {
            self.render_reflog_middle_pane(busy, muted_fg, cx)
        } else {
            self.render_history_middle_pane(fg, muted_fg, accent, border, mono, busy, cx)
        };
        // reflog 行没有完整 commit 元数据；detail 面板对其无意义，强制隐藏
        let show_detail = !self.showing_reflog && self.viewing_commit.is_some();

        // 右半内容：默认仅 commit graph；进入详情时变成内部 h_resizable（middle | detail）
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

        // 外层与上半共用 `ide_left_resize`：两边都是 2 子项（左 / 右半），共享 state
        // → 上下左栏宽度 100% 同步对齐（拖一边另一边跟随，IDEA / VSCode 标准做法）
        gpui_component::resizable::h_resizable("vcs-history-bottom")
            .with_state(&self.ide_left_resize)
            .child(
                gpui_component::resizable::resizable_panel()
                    .size(px(280.0))
                    .size_range(px(220.0)..px(600.0))
                    .child(
                        div()
                            .size_full()
                            .border_r_1()
                            .border_color(border)
                            .child(left),
                    ),
            )
            .child(
                gpui_component::resizable::resizable_panel()
                    .child(div().size_full().child(right_part)),
            )
            .into_any_element()
    }

    /// 中栏（reflog 模式）：搜索行 + 现有 reflog 列表
    /// 与 commit 中栏共用同一空间，左栏 / 整体三栏框架不变
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

    /// 左栏：本地/远程分支 + Tag 三段合并为单个 uniform_list（段表头 + 行 + 新建输入，28px 等高）
    fn render_history_left_pane(&self, _border: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        let rows_rc = self.history_left_rows();
        let total = rows_rc.len();
        let body = uniform_list(
            "vcs-history-left-rows",
            total,
            cx.processor({
                let rows_rc = rows_rc.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_left_row(&rows_rc[i], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.history_left_scroll)
        .flex_1();

        // size_full + min_h_0：在外层定高区内拿到确定高度，uniform_list 自带虚拟滚动
        v_flex()
            .id("vcs-history-left-pane")
            .size_full()
            .min_h_0()
            .px(px(8.0))
            .py(px(6.0))
            .child(body)
            .into_any_element()
    }

    fn history_left_rows(&self) -> Rc<Vec<LeftRow>> {
        let key = HistoryLeftRowsCacheKey {
            local_identity: self.local_branches.as_ptr() as usize,
            local_len: self.local_branches.len(),
            remote_identity: self.remote_branches.as_ptr() as usize,
            remote_len: self.remote_branches.len(),
            tags_identity: self.tags.as_ptr() as usize,
            tags_len: self.tags.len(),
            remotes_identity: self.remotes.as_ptr() as usize,
            remotes_len: self.remotes.len(),
            collapsed_local: self.collapsed_local,
            collapsed_remote: self.collapsed_remote,
            collapsed_tag: self.collapsed_tag,
            collapsed_remote_repos: self.collapsed_remote_repos,
        };
        {
            let cache = self.history_left_rows_cache.borrow();
            if let Some(rows) = cache.as_ref().and_then(|entry| entry.get(&key)) {
                return rows;
            }
        }

        let mut rows: Vec<LeftRow> = Vec::new();

        // 本地分支段：表头 + 行 + 底部新建
        rows.push(LeftRow::Header {
            title: "本地分支",
            count: self.local_branches.len(),
            collapsed: self.collapsed_local,
            section: SidebarSection::Local,
        });
        if !self.collapsed_local {
            for idx in 0..self.local_branches.len() {
                rows.push(LeftRow::Branch {
                    idx,
                    is_remote: false,
                });
            }
            rows.push(LeftRow::CreateBranch);
        }

        // 远程分支段：表头 + 行（空则占位）
        rows.push(LeftRow::Header {
            title: "远程分支",
            count: self.remote_branches.len(),
            collapsed: self.collapsed_remote,
            section: SidebarSection::Remote,
        });
        if !self.collapsed_remote {
            if self.remote_branches.is_empty() {
                rows.push(LeftRow::Empty("暂无远程分支（Fetch 后显示）"));
            } else {
                for idx in 0..self.remote_branches.len() {
                    rows.push(LeftRow::Branch {
                        idx,
                        is_remote: true,
                    });
                }
            }
        }

        // 远程仓库段：表头 + 行（空则占位）+ 底部新建（管 remote 配置，区别于「远程分支」）
        rows.push(LeftRow::Header {
            title: "远程仓库",
            count: self.remotes.len(),
            collapsed: self.collapsed_remote_repos,
            section: SidebarSection::RemoteRepo,
        });
        if !self.collapsed_remote_repos {
            if self.remotes.is_empty() {
                rows.push(LeftRow::Empty("暂无远程仓库（下方输入框添加）"));
            } else {
                for idx in 0..self.remotes.len() {
                    rows.push(LeftRow::Remote { idx });
                }
            }
            rows.push(LeftRow::CreateRemote);
        }

        // Tag 段：表头 + 行（空则占位）+ 底部新建
        rows.push(LeftRow::Header {
            title: "Tag",
            count: self.tags.len(),
            collapsed: self.collapsed_tag,
            section: SidebarSection::Tag,
        });
        if !self.collapsed_tag {
            if self.tags.is_empty() {
                rows.push(LeftRow::Empty("暂无 tag（下方输入框创建）"));
            } else {
                for idx in 0..self.tags.len() {
                    rows.push(LeftRow::Tag { idx });
                }
            }
            rows.push(LeftRow::CreateTag);
        }

        let rows = Rc::new(rows);
        self.history_left_rows_cache
            .replace(Some(HistoryLeftRowsCacheEntry {
                key,
                rows: rows.clone(),
            }));
        rows
    }

    /// 中栏：计数 + 列头 + uniform_list 虚拟化 + 加载更多。列头 / count / footer 在外层非虚拟
    #[allow(clippy::too_many_arguments)]
    fn render_history_middle_pane(
        &self,
        fg: gpui::Hsla,
        muted_fg: gpui::Hsla,
        accent: gpui::Hsla,
        _border: gpui::Hsla,
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
            // 区分「过滤后无结果」与「仓库无提交」，避免误以为仓库是空的
            let filtered = !self.history_search_input.read(cx).value().trim().is_empty()
                || self.history_path_filter.is_some();
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
        // 有更多时加一行哨兵行：滚到底自动触发下一页加载
        let total_rows = count + usize::from(has_more);
        // Rc 共享：commits + graph_rows 喂给 uniform_list 闭包（不每帧 clone 整个 Vec）
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
                                // 哨兵行：滚到底时自动加载下一页
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
                .child("— 已达到历史显示上限，请使用搜索或文件过滤缩小范围 —")
                .into_any_element()
        } else if !has_more {
            div()
                .flex_none()
                .py(px(8.0))
                .flex()
                .justify_center()
                .text_xs()
                .text_color(muted_fg)
                .child("— 已到底 —")
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
