use super::SettingsView;
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
                v_flex()
                    .w_full()
                    .p(px(16.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(8.0))
                    .child(ssh_toggle_row(
                        "settings-ssh-windows-sftp-compatibility",
                        "Windows SFTP 兼容模式",
                        "SSH 可连接，但标准 SFTP 无法访问 Windows 目录时开启。",
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
    description: &'static str,
    checked: bool,
    disabled: bool,
    on_click: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    muted: gpui::Hsla,
) -> impl IntoElement {
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
        .child(
            div()
                .w_full()
                .text_xs()
                .text_color(muted)
                .child(description),
        )
}
