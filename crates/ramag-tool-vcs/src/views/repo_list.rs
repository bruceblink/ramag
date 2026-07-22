//! 仓库管理页：1080px 居中、整行点击=打开、行内按钮独立 emit。空态主按钮「选择本地仓库」

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, input::Input, v_flex,
};

impl VcsView {
    /// self.error 为空时返回 None；不阻塞下方交互
    pub(super) fn render_error_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let err = self.error.as_ref()?;
        let theme = cx.theme();
        let mut banner_bg = theme.danger;
        banner_bg.a = 0.10;
        let danger = theme.danger;
        Some(
            h_flex()
                .w_full()
                .items_start()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(10.0))
                .bg(banner_bg)
                .border_b_1()
                .border_color(danger)
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .small()
                        .text_color(danger),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(danger)
                        .child(err.clone()),
                )
                .child({
                    // 长诊断（网络 / git stderr）单行难读全，支持一键复制出去排查
                    let err_for_copy = err.clone();
                    ramag_ui::clickable_button("vcs-error-copy")
                        .ghost()
                        .xsmall()
                        .label("复制")
                        .on_click(move |_: &ClickEvent, _, app| {
                            app.write_to_clipboard(gpui::ClipboardItem::new_string(
                                err_for_copy.clone(),
                            ));
                        })
                })
                .child(
                    ramag_ui::clickable_button("vcs-error-clear")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.clear_error(cx);
                        })),
                )
                .into_any_element(),
        )
    }
}
use ramag_domain::entities::{RepoConfig, contains_case_insensitive};

use super::vcs_view::VcsView;

/// 内容区最大宽度（与 dbclient connection_list 保持一致）
const CONTENT_MAX_W: f32 = 1080.0;

pub(super) struct RepoListRowsCacheEntry {
    repos: Rc<Vec<RepoConfig>>,
    query_lower: String,
    indices: Rc<Vec<usize>>,
}

impl RepoListRowsCacheEntry {
    fn get(&self, repos: &Rc<Vec<RepoConfig>>, query_lower: &str) -> Option<Rc<Vec<usize>>> {
        (Rc::ptr_eq(&self.repos, repos) && self.query_lower == query_lower)
            .then(|| self.indices.clone())
    }
}

impl VcsView {
    /// 仓库管理页主入口（active_view == RepoList 时由 Render 路由调用）
    pub(super) fn render_repo_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let accent = theme.accent;
        let border = theme.border;
        let row_hover = theme.muted;
        let bg = theme.background;
        // clone/init/add 走 self.loading（见 admin.rs），工作区操作走 self.busy——都要挡
        let busy = self.busy || self.loading || self.directory_picker_busy;

        // 当前搜索关键字（小写）；空 = 不过滤
        let query = self
            .repo_search_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let repos_rc = self.recent_repos.clone();
        let filtered_indices = self.filtered_repo_indices(repos_rc.clone(), &query);
        let total = repos_rc.len();
        let visible_count = filtered_indices.len();

        let header_inner = h_flex()
            .w_full()
            .items_center()
            .gap(px(16.0))
            .child(
                div().flex_1().min_w_0().child(
                    div().max_w(px(360.0)).child(
                        ramag_ui::cleanable_input(
                            &self.repo_search_input,
                            "vcs-repo-search-clear",
                            false,
                            cx,
                        )
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                    ),
                ),
            )
            .child(
                ramag_ui::clickable_button("vcs-repo-clone")
                    .ghost()
                    .small()
                    .icon(ramag_ui::icons::download())
                    .tooltip("克隆")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_clone_panel = !this.show_clone_panel;
                        cx.notify();
                    })),
            )
            .child(
                ramag_ui::clickable_button("vcs-repo-init")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("初始化")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.pick_init_directory(cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("vcs-repo-add")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .tooltip("打开")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.pick_directory(cx);
                    })),
            );

        let header = h_flex()
            .w_full()
            .justify_center()
            .px(px(24.0))
            .pt(px(22.0))
            .pb(px(16.0))
            .border_b_1()
            .border_color(border)
            .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(header_inner));

        let body: AnyElement = if total == 0 {
            empty_state(cx)
        } else if visible_count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(
                    div()
                        .text_sm()
                        .text_color(fg)
                        .child(format!("没有匹配「{query}」的仓库")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_fg)
                        .child("尝试修改关键字或清空搜索"),
                )
                .into_any_element()
        } else {
            let rows = uniform_list(
                "vcs-repo-list-rows",
                visible_count,
                cx.processor({
                    let repos_rc = repos_rc.clone();
                    let filtered_indices = filtered_indices.clone();
                    move |_this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row_index| {
                                let repo_index = filtered_indices[row_index];
                                h_flex()
                                    .w_full()
                                    .justify_center()
                                    .px(px(24.0))
                                    .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(repo_row(
                                        row_index,
                                        &repos_rc[repo_index],
                                        busy,
                                        border,
                                        row_hover,
                                        accent,
                                        fg,
                                        muted_fg,
                                        cx,
                                    )))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }
                }),
            )
            .size_full();
            div()
                .size_full()
                .py(px(10.0))
                .child(rows)
                .into_any_element()
        };

        let mut root = v_flex().size_full().bg(bg);
        if let Some(banner) = self.render_error_banner(cx) {
            root = root.child(banner);
        }
        root.child(header)
            .when(self.show_clone_panel, |c| {
                c.child(self.render_clone_panel(cx))
            })
            .child(body)
            .into_any_element()
    }

    fn filtered_repo_indices(
        &self,
        repos: Rc<Vec<RepoConfig>>,
        query_lower: &str,
    ) -> Rc<Vec<usize>> {
        {
            let cache = self.repo_list_rows_cache.borrow();
            if let Some(indices) = cache
                .as_ref()
                .and_then(|entry| entry.get(&repos, query_lower))
            {
                return indices;
            }
        }

        let mut indices: Vec<usize> = (0..repos.len())
            .filter(|&index| repo_matches_query(&repos[index], query_lower))
            .collect();
        // 收藏优先，其余按最近打开时间倒序（未打开过的按名字排在最后）。
        indices.sort_by(|&left, &right| {
            let left = &repos[left];
            let right = &repos[right];
            right
                .favorite
                .cmp(&left.favorite)
                .then_with(|| right.last_opened_at.cmp(&left.last_opened_at))
                .then_with(|| left.name.cmp(&right.name))
        });
        let indices = Rc::new(indices);
        self.repo_list_rows_cache
            .replace(Some(RepoListRowsCacheEntry {
                repos,
                query_lower: query_lower.to_string(),
                indices: indices.clone(),
            }));
        indices
    }

    /// Clone 面板（`show_clone_panel = true` 时在 header 下方内联展示）
    fn render_clone_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let bg = theme.background;
        let busy = self.loading || self.busy || self.directory_picker_busy;
        let dest_label = self
            .clone_dest_path
            .as_ref()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "点击选择目标目录".into());

        h_flex()
            .w_full()
            .items_center()
            .gap(px(8.0))
            .px(px(24.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(border)
            .bg(bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.clone_url_input).small()),
            )
            .child(
                ramag_ui::clickable_button("vcs-clone-pick-dest")
                    .ghost()
                    .small()
                    .icon(IconName::Folder)
                    .label(dest_label)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.pick_clone_destination(cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("vcs-clone-execute")
                    .primary()
                    .small()
                    .icon(ramag_ui::icons::download())
                    .label("克隆")
                    .disabled(busy || self.clone_dest_path.is_none())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        let url = this.clone_url_input.read(cx).value().trim().to_string();
                        if url.is_empty() {
                            this.error = Some("请输入仓库 URL".into());
                            cx.notify();
                            return;
                        }
                        let Some(dest) = this.clone_dest_path.clone() else {
                            return;
                        };
                        // 目标路径 = 所选父目录 / 从 URL 推导的仓库名
                        let Some(repo_name) = clone_repo_name(&url) else {
                            this.error = Some("无法从 Clone 地址推导仓库名".into());
                            cx.notify();
                            return;
                        };
                        let dest_full = dest.join(repo_name);
                        this.clone_repo_async(url, dest_full, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("vcs-clone-cancel")
                    .ghost()
                    .small()
                    .label("取消")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_clone_panel = false;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

fn repo_matches_query(repo: &RepoConfig, query_lower: &str) -> bool {
    query_lower.is_empty()
        || contains_case_insensitive(&repo.name, query_lower)
        || contains_case_insensitive(&repo.path, query_lower)
}

/// 同时兼容 URL、scp 风格地址和 Windows 本地路径，并拒绝空目录名。
fn clone_repo_name(source: &str) -> Option<String> {
    let source = source.trim().trim_end_matches(['/', '\\']);
    if let Some((_, remainder)) = source.split_once("://")
        && !remainder.contains(['/', '\\'])
    {
        return None;
    }
    let tail = source.rsplit(['/', '\\']).next()?;
    let tail = tail.rsplit(':').next().unwrap_or(tail);
    let name = tail.strip_suffix(".git").unwrap_or(tail);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_string())
}

/// 单条仓库行（整行点击 = 打开；行内删除按钮独立 emit）
#[allow(clippy::too_many_arguments)]
fn repo_row(
    idx: usize,
    r: &RepoConfig,
    busy: bool,
    border: gpui::Hsla,
    hover_bg: gpui::Hsla,
    accent: gpui::Hsla,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    cx: &mut Context<VcsView>,
) -> impl IntoElement {
    // Git 类型 badge（与 dbclient driver 一类一色对齐：Git 用 accent 蓝）
    let badge_fg = accent;
    let mut badge_bg = badge_fg;
    badge_bg.a = 0.12;

    let path_for_open = r.path.clone();
    let path_for_remove = r.path.clone();
    let path_for_favorite = r.path.clone();
    let row_id = SharedString::from(format!("vcs-repo-row-{idx}-{}", r.id));
    let del_id = SharedString::from(format!("vcs-repo-del-{idx}-{}", r.id));
    let favorite_id = SharedString::from(format!("vcs-repo-fav-{idx}-{}", r.id));
    let is_favorite = r.favorite;

    let mono = cx.theme().mono_font_family.clone();

    h_flex()
        .id(row_id)
        .w_full()
        .items_center()
        .gap(px(12.0))
        .px(px(14.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(border)
        .cursor_pointer()
        .hover(move |this| this.bg(hover_bg))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.open_recent_repo(path_for_open.clone(), cx);
        }))
        // 类型 badge（76px 与 dbclient 对齐）
        .child(
            div().flex_none().w(px(76.0)).flex().justify_center().child(
                div()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .text_color(badge_fg)
                    .bg(badge_bg)
                    .child("Git"),
            ),
        )
        // 名称（最重要，flex_1 占主空间，加粗）
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(fg)
                .overflow_hidden()
                .text_ellipsis()
                .child(super::inline_text_preview(&r.name, 160)),
        )
        // 路径（mono 小灰，尾部省略；右对齐占据 360px）
        .child(
            div()
                .flex_none()
                .w(px(360.0))
                .text_xs()
                .text_color(muted_fg)
                .font_family(mono)
                .overflow_hidden()
                .text_ellipsis()
                .child(super::inline_text_preview(&r.path, 240)),
        )
        // 操作按钮组（收藏 + 移除；mouse_down 拦冒泡避免触发整行打开）
        .child(
            h_flex()
                .flex_none()
                .gap(px(4.0))
                .w(px(80.0))
                .justify_end()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    ramag_ui::clickable_button(favorite_id)
                        .ghost()
                        .small()
                        .icon(if is_favorite {
                            IconName::StarFill
                        } else {
                            IconName::Star
                        })
                        .tooltip(if is_favorite {
                            "取消收藏"
                        } else {
                            "收藏"
                        })
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_repo_favorite(path_for_favorite.clone(), cx);
                        })),
                )
                .child(
                    ramag_ui::clickable_button(del_id)
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::trash())
                        .tooltip("移出列表")
                        .disabled(busy)
                        // 弹确认对话框（与 dbclient 删除连接同款交互），用户确认后再真正移除
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.confirm_remove_recent_repo(path_for_remove.clone(), window, cx);
                        })),
                ),
        )
}

/// 空状态：只放一个居中主按钮
fn empty_state(cx: &mut Context<VcsView>) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            ramag_ui::clickable_button("vcs-repo-empty-pick")
                .primary()
                .icon(IconName::Plus)
                .label("打开")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.pick_directory(cx);
                })),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{RepoListRowsCacheEntry, clone_repo_name, repo_matches_query};
    use ramag_domain::entities::RepoConfig;
    use std::rc::Rc;

    #[test]
    fn clone_name_supports_urls_and_windows_paths() {
        assert_eq!(
            clone_repo_name("https://example.com/team/ramag.git"),
            Some("ramag".into())
        );
        assert_eq!(clone_repo_name(r"C:\work\ramag.git"), Some("ramag".into()));
        assert_eq!(
            clone_repo_name("git@example.com:ramag.git"),
            Some("ramag".into())
        );
        assert_eq!(clone_repo_name("https://example.com/"), None);
        assert_eq!(clone_repo_name("/"), None);
    }

    #[test]
    fn repo_search_matches_unicode_without_lowercasing_each_field() {
        let mut repo = RepoConfig::from_path("/tmp/Über-project");
        repo.name = "数据工具".into();

        assert!(repo_matches_query(&repo, "über"));
        assert!(repo_matches_query(&repo, "工具"));
        assert!(!repo_matches_query(&repo, "missing"));
    }

    #[test]
    fn repo_list_cache_requires_same_source_and_query() {
        let repos = Rc::new(vec![RepoConfig::from_path("/tmp/repo")]);
        let indices = Rc::new(vec![0]);
        let cache = RepoListRowsCacheEntry {
            repos: repos.clone(),
            query_lower: "repo".into(),
            indices: indices.clone(),
        };

        let cached = cache.get(&repos, "repo");
        assert!(
            cached
                .as_ref()
                .is_some_and(|value| Rc::ptr_eq(value, &indices))
        );
        assert!(cache.get(&repos, "other").is_none());
        assert!(
            cache
                .get(&Rc::new(repos.as_ref().clone()), "repo")
                .is_none()
        );
    }
}
