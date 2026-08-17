use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel, v_resizable},
    v_flex,
};

use super::helpers::FilesViewMode;
use super::vcs_view::VcsView;
use ramag_ui::PointerDropdownMenu as _;

const TOP_HEIGHT_INITIAL: f32 = 600.0;
const TOP_HEIGHT_MIN: f32 = 200.0;
const TOP_HEIGHT_MAX: f32 = 1400.0;
pub(super) const LEFT_WIDTH_INITIAL: f32 = 280.0;
const LEFT_WIDTH_MIN: f32 = 220.0;
const LEFT_WIDTH_MAX: f32 = 600.0;
const LEFT_WIDTH_PREFERENCE: &str = "git.ide_left_width";

impl VcsView {
    pub(super) fn render_ide_layout(&self, cx: &mut Context<Self>) -> AnyElement {
        let row = h_flex().size_full().min_h_0();
        let mut main_layout = v_flex().size_full();
        if let Some(banner) = self.render_error_banner(cx) {
            main_layout = main_layout.child(banner);
        }
        // 全屏时仅保留标签和差异主区。
        let main_layout = main_layout.child(self.render_op_banner(cx));
        let fullscreen_diff = self.diff_fullscreen
            && !self.show_rebase_plan
            && self.conflict_editor_path.is_none()
            && self
                .active_file_tab_idx
                .and_then(|index| self.file_tabs.get(index))
                .is_some_and(|tab| {
                    !matches!(&tab.source, super::helpers::FileTabSource::ProjectFiles)
                });
        let main_layout = if fullscreen_diff {
            main_layout.child(div().flex_1().min_h_0().child(self.render_main_area(cx)))
        } else if self.history_pane_visible {
            main_layout.child(
                v_resizable("vcs-ide-main")
                    .with_state(&self.ide_files_resize)
                    .child(
                        resizable_panel()
                            .size(px(TOP_HEIGHT_INITIAL))
                            .size_range(px(TOP_HEIGHT_MIN)..px(TOP_HEIGHT_MAX))
                            .child(div().size_full().child(self.render_top_pane(cx))),
                    )
                    .child(
                        resizable_panel()
                            .child(div().size_full().child(self.render_history_pane(cx))),
                    ),
            )
        } else {
            main_layout.child(div().flex_1().min_h_0().child(self.render_top_pane(cx)))
        };

        row.child(div().flex_1().min_w_0().h_full().child(main_layout))
            .into_any_element()
    }

    fn render_top_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let storage = self.storage.clone();
        let view = cx.entity();
        h_resizable("vcs-ide-top")
            .with_state(&self.ide_left_resize)
            .on_resize(move |state, _, cx| {
                persist_ide_left_width(view.clone(), storage.clone(), state, cx);
            })
            .child(
                resizable_panel()
                    .size(px(self.ide_left_width))
                    .size_range(px(LEFT_WIDTH_MIN)..px(LEFT_WIDTH_MAX))
                    .child(
                        div()
                            .id("vcs-files-column")
                            .debug_selector(|| "vcs-files-column".into())
                            .size_full()
                            .border_r_1()
                            .border_color(border)
                            .child(self.render_files_pane(cx)),
                    ),
            )
            .child(
                resizable_panel()
                    .child(div().size_full().min_w_0().child(self.render_main_area(cx))),
            )
            .into_any_element()
    }

    fn render_files_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .child(self.render_files_toolbar(cx))
            // 外层定高、内层滚动，避免长列表被压缩。
            .child(
                div().flex_1().min_h_0().child(
                    div()
                        .size_full()
                        .px(px(10.0))
                        .py(px(6.0))
                        .child(self.render_files_content(cx)),
                ),
            )
            .when(
                matches!(self.files_view_mode, FilesViewMode::Changes),
                |c| c.child(self.render_commit_panel(cx)),
            )
            .into_any_element()
    }

    fn render_files_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let busy = self.busy;
        let active = self.files_view_mode;

        let modes = [
            FilesViewMode::Project,
            FilesViewMode::Changes,
            FilesViewMode::Stash,
        ];
        let mut tabs_row = h_flex().gap(px(2.0)).items_center();
        for mode in modes {
            tabs_row = tabs_row.child(self.mode_tab_button(mode, active, cx));
        }
        let busy_indicator: AnyElement = if let Some(label) = self.busy_label {
            h_flex()
                .flex_1()
                .min_w_0()
                .justify_end()
                .gap(px(4.0))
                .items_center()
                .child(gpui_component::spinner::Spinner::new().xsmall())
                .child(
                    div()
                        .text_xs()
                        .text_color(muted_fg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };
        let mode_row = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .gap(px(8.0))
            .items_center()
            .child(tabs_row)
            .child(busy_indicator)
            .child(self.render_branch_picker(cx));

        let mut search_row = h_flex()
            .w_full()
            .items_center()
            .px(px(10.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .gap(px(6.0))
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(
                        &self.files_search_input,
                        "vcs-files-search-clear",
                        false,
                        cx,
                    )
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(muted_fg)),
                ),
            );
        if self.repo.is_some() {
            search_row = search_row.child(
                ramag_ui::clickable_button("vcs-refresh")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("刷新")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.refresh_workspace_silent(cx);
                    })),
            );
        }
        if matches!(active, FilesViewMode::Project) {
            let any_expanded = !self.project_expanded_dirs.is_empty();
            let (icon, tip) = if any_expanded {
                (IconName::FolderOpen, "折叠")
            } else {
                (IconName::FolderClosed, "展开")
            };
            search_row = search_row.child(
                ramag_ui::clickable_button("vcs-pf-toggle-all")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if any_expanded {
                            this.collapse_all_project_dirs(cx);
                        } else {
                            this.expand_all_project_dirs(cx);
                        }
                    })),
            );
        }
        if matches!(active, FilesViewMode::Changes) {
            search_row = search_row.child(self.render_stash_save_button(cx));
        }
        if self.repo.is_some() {
            let history_visible = self.history_pane_visible;
            search_row = search_row.child(
                ramag_ui::clickable_button("vcs-history-pane-toggle")
                    .ghost()
                    .xsmall()
                    .icon(if history_visible {
                        IconName::PanelBottom
                    } else {
                        IconName::PanelBottomOpen
                    })
                    .tooltip("历史")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_history_pane(cx);
                    })),
            );
        }
        v_flex()
            .child(mode_row)
            .child(search_row)
            .into_any_element()
    }

    fn render_files_content(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.files_view_mode {
            FilesViewMode::Changes => self.render_file_groups(cx),
            FilesViewMode::Project => self.render_project_files_view(cx),
            FilesViewMode::Stash => self.render_stash_view(cx),
        }
    }

    fn render_branch_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.repo.is_none() {
            return div().into_any_element();
        }
        let head = self
            .status
            .as_ref()
            .and_then(|s| s.head_branch.clone())
            .unwrap_or_else(|| "(detached)".into());
        let label = format!("{} ▾", super::inline_text_preview(&head, 80));
        let busy = self.busy;
        let entity = cx.entity();
        let (local_limit, remote_limit) = super::branch_picker::branch_picker_limits(
            self.local_branches.len(),
            self.remote_branches.len(),
        );
        let local: Vec<(String, bool, Option<String>)> = self
            .local_branches
            .iter()
            .filter(|branch| branch.is_head)
            .chain(self.local_branches.iter().filter(|branch| !branch.is_head))
            .take(local_limit)
            .map(|b| {
                let sync = match (b.ahead, b.behind) {
                    (Some(a), Some(d)) if a > 0 || d > 0 => Some(format!("↑{a} ↓{d}")),
                    _ => None,
                };
                (b.name.clone(), b.is_head, sync)
            })
            .collect();
        let remote: Vec<String> = self
            .remote_branches
            .iter()
            .take(remote_limit)
            .map(|b| b.name.clone())
            .collect();
        let total_branches = self
            .local_branches
            .len()
            .saturating_add(self.remote_branches.len());
        let shown_branches = local.len().saturating_add(remote.len());
        let has_head = self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_some();
        ramag_ui::clickable_button("vcs-branch-picker")
            .outline()
            .small()
            .label(label)
            .text_color(cx.theme().foreground)
            .disabled(busy)
            .pointer_dropdown_menu_with_anchor(
                gpui::Anchor::BottomRight,
                move |mut m: PopupMenu, window, cx| {
                    // 父菜单滚动会破坏子菜单，分支组在子菜单内自行滚动。
                    m = m.max_w(px(420.0));
                    if shown_branches < total_branches {
                        m = m.item(PopupMenuItem::label(format!(
                            "快捷菜单仅显示 {shown_branches} / {total_branches} 个分支；完整列表请使用 History 侧栏"
                        )));
                        m = m.separator();
                    }
                    m = m.item(PopupMenuItem::label("操作"));
                    let ent_new = entity.clone();
                    let head_for_dlg = local
                        .iter()
                        .find(|(_, is_head, _)| *is_head)
                        .map(|(n, _, _)| n.clone())
                        .unwrap_or_else(|| "(HEAD)".into());
                    m = m.item(
                        ramag_ui::menu_item_with_disabled("新建分支", !has_head)
                            .on_click({
                                let ent = ent_new.clone();
                                let hdlg = head_for_dlg.clone();
                                let local_for_dlg = local
                                    .iter()
                                    .map(|(n, h, _)| (n.clone(), *h))
                                    .collect::<Vec<_>>();
                                let remote_for_dlg = remote.clone();
                                move |_, window, app| {
                                    super::branch_picker::open_new_branch_dialog(
                                        ent.clone(),
                                        hdlg.clone(),
                                        local_for_dlg.clone(),
                                        remote_for_dlg.clone(),
                                        window,
                                        app,
                                    );
                                }
                            }),
                    );
                    m = m.separator();
                    m = m.item(PopupMenuItem::label("本地"));
                    m = super::branch_picker::render_branches_grouped(
                        m,
                        &local,
                        false,
                        ent_new.clone(),
                        window,
                        cx,
                    );
                    if !remote.is_empty() {
                        m = m.separator();
                        m = m.item(PopupMenuItem::label("远程"));
                        let remote_items: Vec<(String, bool, Option<String>)> =
                            remote.iter().map(|n| (n.clone(), false, None)).collect();
                        m = super::branch_picker::render_branches_grouped(
                            m,
                            &remote_items,
                            true,
                            ent_new.clone(),
                            window,
                            cx,
                        );
                    }
                    m
                },
            )
            .into_any_element()
    }
}

impl VcsView {
    fn mode_tab_button(
        &self,
        mode: FilesViewMode,
        active: FilesViewMode,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = gpui::SharedString::from(format!("vcs-files-tab-{}", mode.id_str()));
        let is_active = mode == active;
        let mut btn = ramag_ui::clickable_button(id)
            .ghost()
            .small()
            .selected(is_active)
            .tooltip(mode.label());
        btn = match mode {
            FilesViewMode::Project => btn.icon(ramag_ui::icons::files()),
            FilesViewMode::Changes => btn.icon(ramag_ui::icons::git_compare()),
            FilesViewMode::Stash => btn.icon(ramag_ui::icons::archive()),
        };
        btn.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.set_files_view_mode(mode, cx);
        }))
        .into_any_element()
    }

    pub(super) fn load_ide_left_width_async(
        storage: std::sync::Arc<dyn ramag_domain::traits::Storage>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let stored = match storage.get_preference(LEFT_WIDTH_PREFERENCE).await {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        operation = "git_layout_width_load",
                        error = %error,
                        "load git layout width failed"
                    );
                    return;
                }
            };
            let width = parse_left_width(stored.as_deref());
            _ = this.update(cx, |this, cx| {
                if (this.ide_left_width - width).abs() < f32::EPSILON {
                    return;
                }
                this.ide_left_width = width;
                // 新状态会在下一次布局时使用持久化宽度初始化面板。
                this.ide_left_resize =
                    cx.new(|_| gpui_component::resizable::ResizableState::default());
                cx.notify();
            });
        })
        .detach();
    }

    fn render_stash_save_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_changes = self
            .status
            .as_ref()
            .map(|s| s.files.iter().any(|f| !f.is_conflicted()))
            .unwrap_or(false);
        ramag_ui::clickable_button("vcs-stash-save")
            .outline()
            .xsmall()
            .icon(IconName::Inbox)
            .tooltip("储藏")
            .disabled(
                self.busy
                    || !has_changes
                    || self
                        .status
                        .as_ref()
                        .and_then(|status| status.operation)
                        .is_some(),
            )
            .on_click(cx.listener(|_this, _: &ClickEvent, window, cx| {
                let entity = cx.entity();
                ramag_ui::open_optional_bounded_prompt(
                    "储藏",
                    "输入 stash 说明（可留空，默认用 git 自动描述）",
                    "",
                    "储藏",
                    ramag_domain::entities::MAX_GIT_STASH_MESSAGE_BYTES,
                    move |msg, _, app| {
                        entity.update(app, |this, cx| this.run_stash_save(msg, cx));
                    },
                    window,
                    cx,
                );
            }))
            .into_any_element()
    }

    fn render_stash_view(&self, cx: &mut Context<Self>) -> AnyElement {
        self.render_stash_list_body(cx)
    }

    fn render_history_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        self.render_history_view(cx)
    }

    fn render_main_area(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.show_rebase_plan {
            return self.render_rebase_plan(cx);
        }
        if self.conflict_editor_path.is_some() {
            return self.render_conflict_editor(cx);
        }
        let is_pf_active = self
            .active_file_tab_idx
            .and_then(|i| self.file_tabs.get(i))
            .map(|t| matches!(t.source, super::helpers::FileTabSource::ProjectFiles))
            .unwrap_or(false);
        let body = if is_pf_active {
            self.render_pf_content(cx)
        } else {
            self.render_diff_block(cx)
        };
        v_flex()
            .size_full()
            .min_w_0()
            .child(self.render_file_tab_bar(cx))
            .child(div().flex_1().min_h_0().min_w_0().w_full().child(body))
            .into_any_element()
    }
}

fn parse_left_width(stored: Option<&str>) -> f32 {
    stored
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(LEFT_WIDTH_INITIAL)
        .clamp(LEFT_WIDTH_MIN, LEFT_WIDTH_MAX)
}

pub(super) fn persist_ide_left_width(
    view: gpui::Entity<VcsView>,
    storage: std::sync::Arc<dyn ramag_domain::traits::Storage>,
    state: &gpui::Entity<gpui_component::resizable::ResizableState>,
    cx: &mut gpui::App,
) {
    let Some(width) = state.read(cx).sizes().first().copied() else {
        return;
    };
    let width = f32::from(width).clamp(LEFT_WIDTH_MIN, LEFT_WIDTH_MAX);
    view.update(cx, |this, _| this.ide_left_width = width);
    cx.background_spawn(async move {
        if let Err(error) = storage
            .set_preference(LEFT_WIDTH_PREFERENCE, &format!("{width:.1}"))
            .await
        {
            tracing::warn!(
                operation = "git_layout_width_save",
                error = %error,
                "persist git layout width failed"
            );
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{LEFT_WIDTH_INITIAL, LEFT_WIDTH_MAX, LEFT_WIDTH_MIN, parse_left_width};

    #[test]
    fn persisted_left_width_is_validated_and_clamped() {
        assert_eq!(parse_left_width(Some("420.5")), 420.5);
        assert_eq!(parse_left_width(Some("100")), LEFT_WIDTH_MIN);
        assert_eq!(parse_left_width(Some("900")), LEFT_WIDTH_MAX);
        assert_eq!(parse_left_width(Some("invalid")), LEFT_WIDTH_INITIAL);
        assert_eq!(parse_left_width(Some("NaN")), LEFT_WIDTH_INITIAL);
        assert_eq!(parse_left_width(None), LEFT_WIDTH_INITIAL);
    }
}
