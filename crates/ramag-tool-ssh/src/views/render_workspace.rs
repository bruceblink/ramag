//! SSH 工作区：SFTP 浏览器与多 Terminal 标签。

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};
use ramag_domain::entities::{RemoteEntry, RemoteEntryKind, SshProfileId};

use super::SshView;

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
                .child("没有打开的 SSH 工作区")
                .child(
                    ramag_ui::clickable_button("ssh-empty-manager")
                        .primary()
                        .label("返回连接管理")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.show_manager(cx);
                        })),
                )
                .into_any_element();
        };
        let workspace_id = workspace.profile.id.clone();
        let main = h_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .items_stretch()
            .child(self.render_file_browser(workspace_id.clone(), cx))
            .child(self.render_terminal_pane(workspace_id, window, cx));
        v_flex()
            .size_full()
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
        let selected_path = workspace.selected_path.clone();
        let loading = workspace.sftp_loading;
        let error = workspace.sftp_error.clone();
        let busy = workspace.operation_busy;
        let connection_available = self.profile_connection_available(&workspace.profile);
        let selected_entry = selected_path
            .as_ref()
            .and_then(|selected| entries.iter().find(|entry| &entry.path == selected));
        let can_download = selected_entry.is_some_and(|entry| entry.kind == RemoteEntryKind::File);
        let has_selection = selected_entry.is_some();
        let border = cx.theme().border;

        let toolbar = h_flex()
            .w_full()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(6.0))
            .bg(cx.theme().secondary)
            .border_b_1()
            .border_color(border)
            .child(tool_button(
                "sftp-up",
                IconName::ArrowUp,
                "返回上级",
                loading || !connection_available,
                cx.listener(|this, _: &ClickEvent, _, cx| this.navigate_parent(cx)),
            ))
            .child(
                ramag_ui::clickable_button("sftp-refresh")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .tooltip("刷新（Cmd/Ctrl+R）")
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
                    .tooltip("上传文件")
                    .disabled(loading || busy || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pick_upload(window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("sftp-download")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::download())
                    .tooltip("下载所选文件")
                    .disabled(!can_download || busy || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.download_selected(window, cx);
                    })),
            )
            .child(tool_button(
                "sftp-mkdir",
                IconName::FolderOpen,
                "新建目录",
                loading || busy || !connection_available,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.prompt_create_directory(window, cx);
                }),
            ))
            .child(
                ramag_ui::clickable_button("sftp-rename")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::pencil())
                    .tooltip("重命名")
                    .disabled(!has_selection || busy || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.prompt_rename_selected(window, cx);
                    })),
            )
            .child(
                ramag_ui::clickable_button("sftp-delete")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::trash())
                    .tooltip("永久删除")
                    .disabled(!has_selection || busy || !connection_available)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.request_delete_selected(window, cx);
                    })),
            );

        let body: AnyElement = if !connection_available && entries.is_empty() {
            centered_message("OpenSSH 不可用；请在连接管理中重新探测或配置自定义路径", cx)
                .into_any_element()
        } else if loading && entries.is_empty() {
            centered_message("正在连接 SFTP 并读取目录…", cx).into_any_element()
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
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("若主机指纹尚未确认，请先在右侧 Terminal 中确认，然后重试。"),
                )
                .child(
                    ramag_ui::clickable_button("retry-sftp")
                        .small()
                        .label("重试 SFTP")
                        .disabled(!connection_available)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.refresh_active_directory(cx);
                        })),
                )
                .into_any_element()
        } else if entries.is_empty() {
            centered_message("当前目录为空", cx).into_any_element()
        } else {
            uniform_list(
                SharedString::from(format!("sftp-directory-{workspace_id}")),
                entries.len(),
                cx.processor({
                    let entries = entries.clone();
                    let selected_path = selected_path.clone();
                    let workspace_id = workspace_id.clone();
                    move |_this, range: Range<usize>, _window, cx| {
                        range
                            .map(|index| {
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
            .w(px(340.0))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(border)
            .child(
                v_flex()
                    .w_full()
                    .min_h(px(52.0))
                    .flex_none()
                    .justify_center()
                    .gap(px(2.0))
                    .px(px(10.0))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("远程目录"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(path),
                    ),
            )
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
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
            .overflow_x_scroll();
        for (terminal_id, fallback_label, terminal) in &terminal_views {
            let id = *terminal_id;
            let id_for_close = id;
            let reconnect_workspace_id = workspace_id.clone();
            let selected = active_terminal_id == Some(id);
            let state = terminal.read(cx);
            let label = state.title().unwrap_or_else(|| fallback_label.to_string());
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
                .border_r_1()
                .border_color(border)
                .cursor_pointer()
                .on_click(cx.listener({
                    let workspace_id = workspace_id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        this.select_terminal(workspace_id.clone(), id, cx);
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
                            .tooltip("新建连接，并保留当前终端输出")
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
                .child(
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
                );
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
                .tooltip("新建 Terminal（Cmd/Ctrl+T）")
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
                                "正在启动系统 SSH…"
                            } else {
                                "工作区已恢复，但不会自动重连或恢复终端输出"
                            }),
                    )
                    .child(
                        ramag_ui::clickable_button("connect-restored-ssh")
                            .primary()
                            .icon(IconName::SquareTerminal)
                            .label("连接并打开 Terminal")
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
            .child(div().flex_1().min_h_0().bg(gpui::rgb(0x1e1e1e)).child(body))
            .into_any_element()
    }
}

fn remote_entry_row(
    entry: RemoteEntry,
    selected_path: Option<&String>,
    workspace_id: SshProfileId,
    index: usize,
    cx: &mut Context<SshView>,
) -> AnyElement {
    let selected = selected_path == Some(&entry.path);
    let icon = match entry.kind {
        RemoteEntryKind::Directory => IconName::Folder,
        RemoteEntryKind::Symlink => IconName::ExternalLink,
        RemoteEntryKind::File | RemoteEntryKind::Other => IconName::File,
    };
    let metadata = format_entry_metadata(&entry);
    let entry_for_click = entry.clone();
    h_flex()
        .id(("sftp-entry", index))
        .w_full()
        .h(px(52.0))
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(if selected {
            cx.theme().muted
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().muted))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.select_remote_entry(workspace_id.clone(), entry_for_click.path.clone(), cx);
            if event.click_count() >= 2 {
                this.activate_remote_entry(
                    workspace_id.clone(),
                    entry_for_click.clone(),
                    window,
                    cx,
                );
            }
        }))
        .child(
            Icon::new(icon)
                .small()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(entry.name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(metadata),
                ),
        )
        .into_any_element()
}

fn format_entry_metadata(entry: &RemoteEntry) -> String {
    let kind = match entry.kind {
        RemoteEntryKind::File => "文件",
        RemoteEntryKind::Directory => "目录",
        RemoteEntryKind::Symlink => "软链接",
        RemoteEntryKind::Other => "其他",
    };
    let size = if entry.kind == RemoteEntryKind::Directory {
        "—".to_string()
    } else {
        format_bytes(entry.size)
    };
    let permissions = entry.permissions.map_or_else(
        || "----".to_string(),
        |mode| format!("{:04o}", mode & 0o7777),
    );
    let modified = entry
        .modified_at
        .as_ref()
        .map_or_else(|| "时间未知".to_string(), |time| time.to_rfc3339());
    format!("{kind}  {size}  {permissions}  {modified}")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn centered_message(message: &'static str, cx: &gpui::App) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message)
}

fn tool_button(
    id: &'static str,
    icon: IconName,
    tooltip: &'static str,
    disabled: bool,
    listener: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    ramag_ui::clickable_button(id)
        .ghost()
        .xsmall()
        .icon(icon)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_are_bounded_and_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert!(format_bytes(u64::MAX).ends_with("TiB"));
    }
}
