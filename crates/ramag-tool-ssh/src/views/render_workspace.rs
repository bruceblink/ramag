//! SSH 工作区：SFTP 浏览器与多 Terminal 标签。

mod file_browser;
use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use ramag_domain::entities::{
    MAX_SSH_TERMINALS_PER_WORKSPACE, RemoteOperatingSystem, RemotePlatformPreference,
    SftpTransportKind, SshProfileId, SshProfileOrigin,
};
use std::ops::Range;

use super::SshView;
use super::model::{SshWorkspace, can_close_terminal};
use super::render_directory_helpers::{
    RemoteDirectoryDrag, RemoteEntryMenuState, centered_message, directory_counts,
    directory_counts_at, filtered_entry_indices, remote_breadcrumbs, remote_entry_row,
};

const FILE_BROWSER_WIDTH_INITIAL: f32 = 280.0;
const FILE_BROWSER_WIDTH_MIN: f32 = 180.0;
const FILE_BROWSER_WIDTH_MAX: f32 = 600.0;

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
        let workspace_resize = self
            .workspace_resizes
            .entry(workspace_id.clone())
            .or_insert_with(|| cx.new(|_| gpui_component::resizable::ResizableState::default()))
            .clone();
        let main = div()
            .id("ssh-workspace-main")
            .debug_selector(|| "ssh-workspace-main".into())
            .size_full()
            .child(
                h_resizable("ssh-workspace-resize")
                    .with_state(&workspace_resize)
                    .child(
                        resizable_panel()
                            .flex_none()
                            .size(px(FILE_BROWSER_WIDTH_INITIAL))
                            .size_range(px(FILE_BROWSER_WIDTH_MIN)..px(FILE_BROWSER_WIDTH_MAX))
                            .child(self.render_file_browser(workspace_id.clone(), cx)),
                    )
                    .child(resizable_panel().child(
                        div().size_full().min_w_0().child(self.render_terminal_pane(
                            workspace_id,
                            window,
                            cx,
                        )),
                    )),
            );
        div()
            .size_full()
            .relative()
            .child(main)
            .child(self.render_transfer_queue(cx))
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
        let link = cx.theme().link;
        let link_hover = cx.theme().link_hover;
        let muted = cx.theme().muted_foreground;
        let path_drag = RemoteDirectoryDrag::from_current_path(workspace_id.clone(), path);
        let workspace_for_prompt = workspace_id.clone();
        let mut path_parts = h_flex()
            .id("ssh-directory-path-scroll")
            .flex_1()
            .min_w_0()
            .gap(px(5.0))
            .overflow_x_scroll();
        for (index, (label, target)) in parts.into_iter().enumerate() {
            if index > 0 {
                path_parts = path_parts.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(SharedString::from("›")),
                );
            }
            let id = SharedString::from(format!("ssh-path-part-{index}"));
            let target_for_click = target.clone();
            let workspace_id_for_click = workspace_id.clone();
            path_parts = path_parts.child(
                div()
                    .id(id)
                    .flex_none()
                    .cursor_pointer()
                    .text_color(link)
                    .when(index == last, |part| {
                        part.font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .hover(move |part| part.text_color(link_hover))
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
        h_flex()
            .id("ssh-directory-breadcrumb")
            .debug_selector(|| "ssh-directory-breadcrumb".into())
            .w_full()
            .h(px(40.0))
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .px(px(10.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(
                div()
                    .id("ssh-directory-path-label")
                    .debug_selector(|| "ssh-directory-path-label".into())
                    .flex_none()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |label| label.text_color(link_hover))
                    .child("路径")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.prompt_remote_path(workspace_for_prompt.clone(), window, cx);
                    })),
            )
            .child(path_parts)
            .when_some(path_drag, |breadcrumb, drag| {
                breadcrumb
                    .cursor_pointer()
                    .on_drag(drag, |drag, position, _, cx| {
                        cx.new(|_| drag.clone().position(position))
                    })
            })
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
        let production = workspace.profile.production;
        let connection_available = self.profile_connection_available(&workspace.profile);
        let active_terminal_id = workspace.active_terminal_id;
        let terminal_views = workspace
            .terminals
            .iter()
            .map(|terminal| (terminal.id, terminal.label.clone(), terminal.view.clone()))
            .collect::<Vec<_>>();
        let terminals_can_close = can_close_terminal(terminal_views.len());
        let terminal_limit_reached = terminal_views.len() >= MAX_SSH_TERMINALS_PER_WORKSPACE;
        let border = cx.theme().border;
        let secondary = cx.theme().secondary;
        let muted_bg = cx.theme().muted;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let accent = cx.theme().accent;
        let mut drop_background = accent;
        drop_background.a = 0.08;
        let warning = cx.theme().warning;

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
        for (terminal_id, fallback_label, terminal) in &terminal_views {
            let id = *terminal_id;
            let id_for_close = id;
            let reconnect_workspace_id = workspace_id.clone();
            let selected = active_terminal_id == Some(id);
            let state = terminal.read(cx);
            let label = fallback_label.to_string();
            let exited = state.core().exit_status();
            let can_reconnect = exited.is_some();
            let display = match exited {
                Some(status) => format!(
                    "{label} [退出{}]",
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
                            .disabled(terminal_loading || !connection_available)
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
                .when(terminals_can_close, |tab| {
                    tab.child(
                        ramag_ui::clickable_button(("close-ssh-terminal", id_for_close))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener({
                                let workspace_id = workspace_id.clone();
                                move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_terminal(
                                        workspace_id.clone(),
                                        id_for_close,
                                        window,
                                        cx,
                                    );
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
                .disabled(terminal_loading || terminal_limit_reached || !connection_available)
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

        let empty_workspace_id = workspace_id.clone();
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
                                "连接中…"
                            } else {
                                "未连接"
                            }),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("close-empty-ssh-workspace")
                                    .debug_selector(|| "close-empty-ssh-workspace".into())
                                    .child(
                                        ramag_ui::clickable_button(
                                            "close-empty-ssh-workspace-button",
                                        )
                                        .outline()
                                        .icon(IconName::Close)
                                        .label("关闭连接")
                                        .on_click(
                                            cx.listener(move |this, _: &ClickEvent, window, cx| {
                                                this.request_close_workspace(
                                                    empty_workspace_id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
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
                            ),
                    )
                    .into_any_element()
            });
        let can_drop_workspace = workspace_id.clone();
        let style_workspace = workspace_id.clone();
        let dropped_workspace = workspace_id.clone();
        v_flex()
            .id("ssh-terminal-drop-target")
            .debug_selector(|| "ssh-terminal-drop-target".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .can_drop(move |value, _, _| {
                value
                    .downcast_ref::<RemoteDirectoryDrag>()
                    .is_some_and(|drag| drag.workspace_id == can_drop_workspace)
            })
            .drag_over(move |style, drag: &RemoteDirectoryDrag, _, _| {
                if drag.workspace_id == style_workspace {
                    style.border_2().border_color(accent).bg(drop_background)
                } else {
                    style
                }
            })
            .on_drop(
                cx.listener(move |this, drag: &RemoteDirectoryDrag, window, cx| {
                    if drag.workspace_id == dropped_workspace {
                        this.start_terminal_in_directory(
                            dropped_workspace.clone(),
                            drag.path.clone(),
                            window,
                            cx,
                        );
                    }
                }),
            )
            .child(tabs)
            .when(production, |pane| {
                pane.child(
                    h_flex().w_full().flex_none().px(px(8.0)).py(px(3.0)).child(
                        div()
                            .id("ssh-production-terminal-warning")
                            .debug_selector(|| "ssh-production-terminal-warning".into())
                            .flex_none()
                            .text_xs()
                            .text_color(warning)
                            .child("终端未限制生产只读，请谨慎操作！"),
                    ),
                )
            })
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

fn empty_directory_message(workspace: &SshWorkspace) -> &'static str {
    let windows = workspace.profile.remote_platform == RemotePlatformPreference::Windows
        || workspace.capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.operating_system == RemoteOperatingSystem::Windows
        });
    if workspace.directory_loaded
        && workspace.profile.origin == SshProfileOrigin::JumpServer
        && windows
        && workspace.path == "/"
    {
        "未返回可访问盘符"
    } else {
        "目录为空"
    }
}

#[cfg(test)]
mod tests;
