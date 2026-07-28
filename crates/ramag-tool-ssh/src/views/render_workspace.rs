//! SSH 工作区：SFTP 浏览器与多 Terminal 标签。

use gpui::{
    Anchor, AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, menu::PopupMenu, v_flex,
};
use ramag_domain::entities::{RemoteEntryKind, SshProfileId};
use ramag_ui::PointerDropdownMenu as _;
use std::ops::Range;

use super::SshView;
use super::render_directory_helpers::{
    centered_message, directory_counts, directory_counts_at, filtered_entry_indices,
    remote_breadcrumbs, remote_entry_row,
};

const FILE_BROWSER_WIDTH: f32 = 280.0;

impl SshView {
    pub(super) fn render_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(workspace) = self.active_workspace() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child("暂无连接")
                .child(
                    ramag_ui::clickable_button("ssh-empty-manager")
                        .primary()
                        .label("返回")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.show_manager(cx);
                        })),
                )
                .into_any_element();
        };
        let workspace_id = workspace.profile.id.clone();
        let main = h_flex()
            .id("ssh-workspace-main")
            .debug_selector(|| "ssh-workspace-main".into())
            .size_full()
            .items_stretch()
            .child(self.render_file_browser(workspace_id.clone(), cx))
            .child(self.render_terminal_pane(workspace_id, window, cx));
        div()
            .size_full()
            .relative()
            .child(main)
            .child(self.render_transfer_queue(cx))
            .into_any_element()
    }

    fn render_file_browser(
        &self,
        workspace_id: SshProfileId,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
        else {
            return div().into_any_element();
        };
        let path = workspace.path.clone();
        let entries = workspace.entries.clone();
        let filtered_indices = filtered_entry_indices(&entries, &workspace.directory_query);
        let visible_len = filtered_indices
            .as_ref()
            .map_or(entries.len(), |indices| indices.len());
        let selected_path = workspace.selected_path.clone();
        let loading = workspace.sftp_loading;
        let error = workspace.sftp_error.clone();
        let busy = workspace.operation_busy;
        let sftp_locked = workspace.profile.production;
        let connection_available = self.profile_connection_available(&workspace.profile);
        let selected_entry = selected_path
            .as_ref()
            .and_then(|selected| entries.iter().find(|entry| &entry.path == selected));
        let can_download = selected_entry.is_some_and(|entry| entry.kind == RemoteEntryKind::File);
        let has_selection = selected_entry.is_some();
        let (total_directories, total_files) = directory_counts(&entries);
        let (visible_directories, visible_files) = filtered_indices
            .as_ref()
            .map_or((total_directories, total_files), |indices| {
                directory_counts_at(&entries, indices)
            });
        let summary = if filtered_indices.is_some() {
            format!(
                "目录 {visible_directories}/{total_directories} · 文件 {visible_files}/{total_files}"
            )
        } else {
            format!("目录 {total_directories} · 文件 {total_files}")
        };
        let has_transfers = self
            .service
            .transfer_tasks()
            .iter()
            .any(|task| task.profile_id == workspace_id);
        let transfers_visible = workspace.transfers_visible;
        let border = cx.theme().border;

        let menu_entity = cx.entity();
        let can_create = !loading && !busy && !sftp_locked && connection_available;
        let can_rename = has_selection && !busy && !sftp_locked && connection_available;
        let can_delete = can_rename;
        let show_file_actions = has_selection && !sftp_locked && connection_available;
        let show_more = show_file_actions || has_transfers;
        let toolbar = h_flex()
            .w_full()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .px(px(6.0))
            .bg(cx.theme().secondary)
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id("ssh-directory-search")
                    .debug_selector(|| "ssh-directory-search".into())
                    .flex_1()
                    .min_w_0()
                    .child(
                        ramag_ui::cleanable_input(
                            &self.directory_search,
                            "ssh-directory-search-clear",
                            false,
                            cx,
                        )
                        .small()
                        .prefix(
                            Icon::new(IconName::Search)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        ),
                    ),
            )
            .child(
                ramag_ui::clickable_button("sftp-refresh")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("刷新")
                    .disabled(loading || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.refresh_active_directory(cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("sftp-upload")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::upload())
                    .tooltip("上传")
                    .disabled(loading || busy || sftp_locked || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pick_upload(window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("sftp-download")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::download())
                    .tooltip("下载")
                    .disabled(!can_download || busy || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.download_selected(window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("sftp-mkdir")
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .tooltip("新建目录")
                    .disabled(!can_create)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.prompt_create_directory(window, cx);
                    })),
            )
            .when(show_more, |toolbar| {
                toolbar.child(
                    div()
                        .id("ssh-directory-more-trigger")
                        .debug_selector(|| "ssh-directory-more-trigger".into())
                        .child(
                            ramag_ui::clickable_button("ssh-directory-more")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Ellipsis)
                                .pointer_dropdown_menu_with_anchor(
                                    Anchor::TopLeft,
                                    move |mut menu: PopupMenu, _, _| {
                                        if show_file_actions {
                                            let rename_entity = menu_entity.clone();
                                            menu = menu.item(
                                                ramag_ui::menu_item_with_disabled(
                                                    "改名",
                                                    !can_rename,
                                                )
                                                .on_click(move |_, window, app| {
                                                    rename_entity.update(app, |this, cx| {
                                                        this.prompt_rename_selected(window, cx);
                                                    });
                                                }),
                                            );
                                            let delete_entity = menu_entity.clone();
                                            menu = menu.item(
                                                ramag_ui::menu_item_with_disabled(
                                                    "删除",
                                                    !can_delete,
                                                )
                                                .on_click(move |_, window, app| {
                                                    delete_entity.update(app, |this, cx| {
                                                        this.request_delete_selected(window, cx);
                                                    });
                                                }),
                                            );
                                        }
                                        if has_transfers {
                                            if show_file_actions {
                                                menu = menu.separator();
                                            }
                                            let transfer_entity = menu_entity.clone();
                                            menu = menu.item(
                                                ramag_ui::menu_item(if transfers_visible {
                                                    "收起传输"
                                                } else {
                                                    "查看传输"
                                                })
                                                .on_click(move |_, _, app| {
                                                    transfer_entity.update(app, |this, cx| {
                                                        this.toggle_transfer_panel(cx);
                                                    });
                                                }),
                                            );
                                        }
                                        menu
                                    },
                                ),
                        ),
                )
            });

        let body: AnyElement = if !connection_available && entries.is_empty() {
            centered_message("OpenSSH 不可用，请编辑连接", cx).into_any_element()
        } else if loading && entries.is_empty() {
            centered_message("加载目录…", cx).into_any_element()
        } else if let Some(error) = error {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .px(px(14.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("SFTP 连接失败：{error}")),
                )
                .child(
                    ramag_ui::clickable_button("retry-sftp")
                        .small()
                        .label("重试")
                        .disabled(!connection_available)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.refresh_active_directory(cx);
                        })),
                )
                .into_any_element()
        } else if entries.is_empty() {
            centered_message("目录为空", cx).into_any_element()
        } else if visible_len == 0 {
            centered_message("暂无匹配", cx).into_any_element()
        } else {
            uniform_list(
                SharedString::from(format!("sftp-directory-{workspace_id}")),
                visible_len,
                cx.processor({
                    let entries = entries.clone();
                    let filtered_indices = filtered_indices.clone();
                    let selected_path = selected_path.clone();
                    let workspace_id = workspace_id.clone();
                    move |_this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row_index| {
                                let index = filtered_indices
                                    .as_ref()
                                    .map_or(row_index, |indices| indices[row_index]);
                                remote_entry_row(
                                    entries[index].clone(),
                                    selected_path.as_ref(),
                                    workspace_id.clone(),
                                    index,
                                    cx,
                                )
                            })
                            .collect::<Vec<_>>()
                    }
                }),
            )
            .size_full()
            .into_any_element()
        };

        v_flex()
            .id("ssh-file-browser")
            .debug_selector(|| "ssh-file-browser".into())
            .w(px(FILE_BROWSER_WIDTH))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(border)
            .child(self.render_directory_breadcrumb(workspace_id.clone(), &path, cx))
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .child(
                h_flex()
                    .id("ssh-directory-summary")
                    .debug_selector(|| "ssh-directory-summary".into())
                    .w_full()
                    .h(px(32.0))
                    .flex_none()
                    .items_center()
                    .px(px(10.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
            .into_any_element()
    }

    fn render_directory_breadcrumb(
        &self,
        workspace_id: SshProfileId,
        path: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let parts = remote_breadcrumbs(path);
        let last = parts.len().saturating_sub(1);
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let mut breadcrumb = h_flex()
            .id("ssh-directory-breadcrumb")
            .debug_selector(|| "ssh-directory-breadcrumb".into())
            .w_full()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .px(px(10.0))
            .overflow_x_scroll()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs();
        for (index, (label, target)) in parts.into_iter().enumerate() {
            if index > 0 {
                breadcrumb = breadcrumb.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(SharedString::from("›")),
                );
            }
            let id = SharedString::from(format!("ssh-path-part-{index}"));
            let target_for_click = target.clone();
            let workspace_id_for_click = workspace_id.clone();
            breadcrumb = breadcrumb.child(
                div()
                    .id(id)
                    .flex_none()
                    .cursor_pointer()
                    .text_color(if index == last { foreground } else { muted })
                    .when(index == last, |part| {
                        part.font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .hover(move |part| part.text_color(foreground))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.refresh_directory(
                            workspace_id_for_click.clone(),
                            Some(target_for_click.clone()),
                            cx,
                        );
                    })),
            );
        }
        breadcrumb
    }

    fn render_terminal_pane(
        &self,
        workspace_id: SshProfileId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.profile_id() == &workspace_id)
        else {
            return div().into_any_element();
        };
        let terminal_loading = workspace.terminal_loading;
        let connection_available = self.profile_connection_available(&workspace.profile);
        let active_terminal_id = workspace.active_terminal_id;
        let terminal_views = workspace
            .terminals
            .iter()
            .map(|terminal| (terminal.id, terminal.label.clone(), terminal.view.clone()))
            .collect::<Vec<_>>();
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let muted_bg = cx.theme().muted;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let accent = cx.theme().accent;

        let mut tabs_strip = h_flex()
            .id(SharedString::from(format!(
                "ssh-terminal-tabs-{workspace_id}"
            )))
            .flex_1()
            .min_w_0()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(2.0))
            .overflow_x_scroll();
        for (tab_index, (terminal_id, fallback_label, terminal)) in
            terminal_views.iter().enumerate()
        {
            let id = *terminal_id;
            let id_for_close = id;
            let primary = tab_index == 0;
            let reconnect_workspace_id = workspace_id.clone();
            let selected = active_terminal_id == Some(id);
            let state = terminal.read(cx);
            let label = fallback_label.to_string();
            let exited = state.core().exit_status();
            let can_reconnect = exited.is_some();
            let display = match exited {
                Some(status) => format!(
                    "{label} [已退出{}]",
                    status
                        .code
                        .map_or_else(String::new, |code| format!(": {code}"))
                ),
                None => label,
            };
            let mut tab = h_flex()
                .id(("ssh-terminal-tab", id))
                .flex_none()
                .max_w(px(260.0))
                .items_center()
                .gap_2()
                .px_3()
                .py(px(7.0))
                .border_1()
                .border_color(border)
                .rounded(px(4.0))
                .cursor_pointer()
                .on_click(cx.listener({
                    let workspace_id = workspace_id.clone();
                    move |this, _: &ClickEvent, window, cx| {
                        this.select_terminal(workspace_id.clone(), id, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(if selected { foreground } else { muted })
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(display),
                )
                .when(can_reconnect, |tab| {
                    tab.child(
                        ramag_ui::clickable_button(("reconnect-ssh-terminal", id))
                            .ghost()
                            .xsmall()
                            .label("重连")
                            .tooltip("保留输出")
                            .disabled(!connection_available)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.reconnect_terminal(
                                    reconnect_workspace_id.clone(),
                                    id,
                                    window,
                                    cx,
                                );
                            })),
                    )
                })
                .when(!primary, |tab| {
                    tab.child(
                        ramag_ui::clickable_button(("close-ssh-terminal", id_for_close))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener({
                                let workspace_id = workspace_id.clone();
                                move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.close_terminal(workspace_id.clone(), id_for_close, cx);
                                }
                            })),
                    )
                });
            if selected {
                let mut active_bg = accent;
                active_bg.a = 0.15;
                tab = tab.bg(active_bg);
            } else {
                tab = tab.hover(move |tab| tab.bg(muted_bg));
            }
            tabs_strip = tabs_strip.child(tab);
        }
        tabs_strip = tabs_strip.child(
            ramag_ui::clickable_button("new-ssh-terminal")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip("新建")
                .disabled(terminal_loading || !connection_available)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.start_active_terminal(window, cx);
                })),
        );
        let tabs = h_flex()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .bg(secondary)
            .child(tabs_strip);

        let body = terminal_views
            .iter()
            .find(|(id, _, _)| Some(*id) == active_terminal_id)
            .map(|(_, _, terminal)| terminal.clone().into_any_element())
            .unwrap_or_else(|| {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if terminal_loading {
                                "正在连接"
                            } else {
                                "尚未连接"
                            }),
                    )
                    .child(
                        ramag_ui::clickable_button("connect-restored-ssh")
                            .primary()
                            .icon(IconName::SquareTerminal)
                            .label("连接")
                            .disabled(terminal_loading || !connection_available)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.connect_active_workspace(window, cx);
                            })),
                    )
                    .into_any_element()
            });
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .bg(cx.theme().background)
                    .child(body),
            )
            .into_any_element()
    }
}
