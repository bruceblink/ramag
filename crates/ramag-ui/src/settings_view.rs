//! 全局设置中心：剪贴板总开关（热键 / 图片采集 / 排除应用等细项在剪贴板工具内管理）。

use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, WindowExt as _, h_flex, notification::Notification,
    scroll::ScrollableElement as _, v_flex,
};
use ramag_app::ClipboardService;
use ramag_domain::entities::ClipboardSettings;

use crate::platform::clipboard_hotkey;

pub struct SettingsView {
    clipboard_service: Arc<ClipboardService>,
    clipboard: ClipboardSettings,
    loaded_revision: u64,
    saving_clipboard: bool,
    pending_notification: Option<Notification>,
}

impl SettingsView {
    pub fn new(clipboard_service: Arc<ClipboardService>, cx: &mut Context<Self>) -> Self {
        let (clipboard, loaded_revision) = clipboard_service.settings_snapshot_with_revision();

        let service = clipboard_service.clone();
        cx.spawn(async move |this, cx| {
            service.load_settings().await;
            let (settings, revision) = service.settings_snapshot_with_revision();
            let _ = this.update(cx, |this, cx| {
                if !this.saving_clipboard {
                    this.clipboard = settings;
                    this.loaded_revision = revision;
                    cx.notify();
                }
            });
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(600))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        let revision = this.clipboard_service.settings_revision();
                        if !this.saving_clipboard && revision != this.loaded_revision {
                            let (settings, revision) =
                                this.clipboard_service.settings_snapshot_with_revision();
                            this.clipboard = settings;
                            this.loaded_revision = revision;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            clipboard_service,
            clipboard,
            loaded_revision,
            saving_clipboard: false,
            pending_notification: None,
        }
    }

    fn save_clipboard(&mut self, next: ClipboardSettings, cx: &mut Context<Self>) {
        if self.saving_clipboard || self.clipboard == next {
            return;
        }
        let previous = std::mem::replace(&mut self.clipboard, next.clone());
        self.saving_clipboard = true;
        cx.notify();

        let service = self.clipboard_service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.save_settings(&next).await;
            let _ = this.update(cx, |this, cx| {
                this.saving_clipboard = false;
                match result {
                    Ok(()) => {
                        let (settings, revision) = service.settings_snapshot_with_revision();
                        this.loaded_revision = revision;
                        this.clipboard = settings;
                    }
                    Err(error) => {
                        this.clipboard = previous;
                        this.pending_notification = Some(Notification::error(format!(
                            "剪贴板设置保存失败（已还原）：{error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(notification) = self.pending_notification.take() {
            window.push_notification(notification, cx);
        }
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let clipboard = self.clipboard.clone();
        let clipboard_disabled = self.saving_clipboard;

        v_flex()
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(820.0))
                    .mx_auto()
                    .p(px(28.0))
                    .gap(px(24.0))
                    .child(div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child("设置"))
                    .child(
                        settings_card("剪贴板", border)
                            .when(self.clipboard_service.settings_degraded(), |card| {
                                card.child(div().text_xs().text_color(gpui::red()).child("设置读取异常，采集已自动暂停；重新保存开关可尝试修复。"))
                            })
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(div().text_sm().child("启用剪贴板"))
                                            .child(div().text_xs().text_color(muted).child(format!(
                                                "侧边栏显示入口并后台记录剪贴历史，全局热键 {}；细项在剪贴板工具内设置",
                                                clipboard_hotkey(clipboard.alternate_hotkey)
                                            ))),
                                    )
                                    .child(
                                        crate::clickable_switch("settings-clip-enabled")
                                            .flex_none()
                                            .checked(clipboard.enabled)
                                            .disabled(clipboard_disabled)
                                            .on_click(cx.listener(|this, _: &bool, _, cx| {
                                                let mut next = this.clipboard.clone();
                                                next.enabled = !next.enabled;
                                                this.save_clipboard(next, cx);
                                            })),
                                    ),
                            ),
                    ),
            )
    }
}

fn settings_card(title: &'static str, border: gpui::Hsla) -> gpui::Div {
    v_flex()
        .w_full()
        .p(px(16.0))
        .gap(px(12.0))
        .border_1()
        .border_color(border)
        .rounded(px(8.0))
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
}
