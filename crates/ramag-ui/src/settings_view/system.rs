//! 系统级窗口行为设置的交互与展示。

use super::{SettingsView, pages::settings_card};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{ActiveTheme, Disableable as _, h_flex, notification::Notification, v_flex};
use tracing::error;

impl SettingsView {
    /// 渲染系统设置页面；托盘开关只在 Windows 上可操作，其他平台不改变现有关闭行为。
    pub(super) fn render_system_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let supported = cfg!(target_os = "windows");
        let description = if supported {
            "关闭主窗口后隐藏到任务栏托盘，应用继续在后台运行；可从托盘重新打开。"
        } else {
            "此设置仅在 Windows 上生效，当前平台继续使用默认的关闭行为。"
        };

        settings_card("窗口关闭行为", theme.border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(16.0))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(div().text_sm().child("关闭时最小化到任务栏托盘"))
                            .child(div().text_xs().text_color(muted).child(description)),
                    )
                    .child(
                        crate::clickable_switch("settings-system-minimize-to-tray")
                            .flex_none()
                            .checked(self.system_settings.minimize_to_tray)
                            .disabled(!supported)
                            .on_click(cx.listener(|this, _: &bool, _, cx| {
                                this.toggle_minimize_to_tray(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// 切换托盘驻留设置，先更新全局状态供关闭回调读取，再异步保存最后一次选择。
    pub(super) fn toggle_minimize_to_tray(&mut self, cx: &mut Context<Self>) {
        let next = crate::SystemSettings {
            minimize_to_tray: !self.system_settings.minimize_to_tray,
        };
        match next.to_json() {
            Ok(json) => {
                self.system_settings = next;
                crate::set_system_settings(next, cx);
                crate::preferences::persist_preference_latest(
                    crate::SYSTEM_SETTINGS_PREF_KEY,
                    json,
                    cx,
                );
            }
            Err(error) => {
                error!(
                    operation = "system_settings_save",
                    error = %error,
                    "serialize system settings failed"
                );
                self.pending_notification = Some(Notification::error(error));
            }
        }
        cx.notify();
    }
}
