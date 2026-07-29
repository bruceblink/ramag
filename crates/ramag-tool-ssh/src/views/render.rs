//! SSH 根布局、工作区标签与快捷键入口。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex,
    notification::Notification, v_flex,
};

use super::SshView;
use super::model::ViewMode;
use crate::{CloseSshTerminal, NewSshTerminal, RefreshSftp};

impl SshView {
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let accent = theme.accent;
        let muted_bg = theme.muted;
        let manager_selected = self.view_mode == ViewMode::Manager;
        let mut manager_tab = h_flex()
            .id("ssh-manager-tab")
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py(px(7.0))
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_manager(cx);
            }))
            .child(
                gpui_component::Icon::new(IconName::Network)
                    .small()
                    .text_color(if manager_selected { fg } else { muted }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if manager_selected { fg } else { muted })
                    .child("连接"),
            );
        if manager_selected {
            let mut active_bg = accent;
            active_bg.a = 0.15;
            manager_tab = manager_tab.bg(active_bg);
        } else {
            manager_tab = manager_tab.hover(move |tab| tab.bg(muted_bg));
        }

        let mut workspace_strip = h_flex()
            .id("ssh-workspace-tabs-scroll")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll();
        for workspace in &self.workspaces {
            let id = workspace.profile.id.clone();
            let id_for_close = id.clone();
            let selected = self.view_mode == ViewMode::Workspace
                && self.active_workspace_id.as_ref() == Some(&id);
            let label = workspace.profile.name.clone();
            let dot_color = if workspace.terminal_loading
                || workspace.sftp_loading
                || workspace.file_preview_loading
            {
                gpui::hsla(45.0 / 360.0, 0.9, 0.55, 1.0)
            } else if workspace.sftp_error.is_some() || workspace.profile.production {
                theme.danger
            } else {
                workspace
                    .profile
                    .environment
                    .as_deref()
                    .map(|environment| {
                        super::render_manager::environment_badge_colors(environment, muted).0
                    })
                    .unwrap_or(muted)
            };
            let mut tab = h_flex()
                .id(SharedString::from(format!("ssh-workspace-tab-{id}")))
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(7.0))
                .border_r_1()
                .border_color(border)
                .cursor_pointer()
                .child(div().size(px(8.0)).rounded_full().bg(dot_color))
                .child(
                    div()
                        .text_xs()
                        .text_color(if selected { fg } else { muted })
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(if workspace.profile.production {
                            theme.danger
                        } else {
                            muted
                        })
                        .child(if workspace.profile.production {
                            "生产"
                        } else {
                            "SSH"
                        }),
                )
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "close-ssh-workspace-{id_for_close}"
                    )))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.request_close_workspace(id_for_close.clone(), window, cx);
                        },
                    )),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_workspace(id.clone(), window, cx);
                }));
            if selected {
                let mut active_bg = accent;
                active_bg.a = 0.15;
                tab = tab.bg(active_bg);
            } else {
                tab = tab.hover(move |tab| tab.bg(muted_bg));
            }
            workspace_strip = workspace_strip.child(tab);
        }

        h_flex()
            .id("ssh-workspace-tabs")
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .bg(theme.secondary)
            .child(manager_tab)
            .child(workspace_strip)
    }
}

impl Render for SshView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notice) = self.notice.take() {
            let notification = if notice.error {
                Notification::error(notice.message)
            } else {
                Notification::success(notice.message)
            };
            window.push_notification(notification, cx);
        }
        let body = match self.view_mode {
            ViewMode::Manager => self.render_manager(window, cx).into_any_element(),
            ViewMode::Workspace => self.render_workspace(window, cx).into_any_element(),
        };
        v_flex()
            .key_context("SshWorkspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &NewSshTerminal, window, cx| {
                this.start_active_terminal(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &CloseSshTerminal, _window, cx| {
                this.close_active_terminal(cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &RefreshSftp, _window, cx| {
                this.refresh_active_directory(cx);
                cx.stop_propagation();
            }))
            .child(self.render_tabs(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
