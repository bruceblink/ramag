//! SSH 模块级设置页。

use super::{SettingsView, pages::settings_card};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, Disableable as _, h_flex, v_flex};
use ramag_domain::entities::SshModuleSettings;

impl SettingsView {
    pub(super) fn render_ssh_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let disabled = self.saving_ssh_module_settings;
        let settings = self.ssh_module_settings;

        v_flex()
            .w_full()
            .gap(px(16.0))
            .when(disabled, |page| {
                page.child(div().text_xs().text_color(muted).child("保存中…"))
            })
            .child(
                settings_card("SFTP 兼容", theme.border).child(ssh_toggle_row(
                    "settings-ssh-windows-sftp-compatibility",
                    "Windows SFTP 兼容模式",
                    [
                        "仅当 Windows 远端使用标准 SFTP 无法列出目录或盘符时开启。",
                        "开启后将通过 SSH 启动 Windows OpenSSH 的 sftp-server.exe。",
                        "Linux 或标准 Windows SFTP 正常时无需开启。",
                    ],
                    settings.windows_sftp_compatibility,
                    disabled,
                    cx.listener(|this, _: &bool, _, cx| {
                        let mut next = this.ssh_module_settings;
                        next.windows_sftp_compatibility = !next.windows_sftp_compatibility;
                        this.save_ssh_module_settings(next, cx);
                    }),
                    muted,
                )),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(muted)
                    .child("这是 SSH 模块级开关，会影响所有 Windows SSH/SFTP 连接；修改后已有目录会在下一次请求时按新通道重新建立。"),
            )
            .into_any_element()
    }

    fn save_ssh_module_settings(&mut self, next: SshModuleSettings, cx: &mut Context<Self>) {
        if self.saving_ssh_module_settings || self.ssh_module_settings == next {
            return;
        }
        let previous = self.ssh_module_settings;
        self.ssh_module_settings = next;
        self.saving_ssh_module_settings = true;
        cx.notify();

        let service = self.ssh_service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.save_module_settings(&next).await;
            let _ = this.update(cx, |this, cx| {
                this.saving_ssh_module_settings = false;
                if let Err(error) = result {
                    this.ssh_module_settings = previous;
                    this.pending_notification =
                        Some(gpui_component::notification::Notification::error(format!(
                            "SSH 模块设置保存失败（已还原）：{error}"
                        )));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[allow(clippy::too_many_arguments)]
fn ssh_toggle_row(
    id: &'static str,
    title: &'static str,
    description_lines: [&'static str; 3],
    checked: bool,
    disabled: bool,
    on_click: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let mut description = v_flex().w_full().gap(px(3.0));
    for line in description_lines {
        description = description.child(div().w_full().text_xs().text_color(muted).child(line));
    }
    v_flex()
        .w_full()
        .gap(px(6.0))
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap(px(16.0))
                .child(
                    div()
                        .text_sm()
                        .when(disabled, |row| row.text_color(muted))
                        .child(title),
                )
                .child(
                    crate::clickable_switch(id)
                        .flex_none()
                        .checked(checked)
                        .disabled(disabled)
                        .on_click(on_click),
                ),
        )
        .child(description)
}
