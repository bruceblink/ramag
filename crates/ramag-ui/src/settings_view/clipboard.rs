//! 剪贴板模块设置页。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_app::HotkeyState;
use ramag_domain::entities::{ClipboardSettings, MAX_CLIPBOARD_BLACKLIST_ENTRIES};

use super::{SettingsView, pages::settings_card};
use crate::platform::{auto_paste_description, clipboard_hotkey};

impl SettingsView {
    pub(super) fn render_clipboard_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let settings = self.clipboard.clone();
        let disabled = self.saving_clipboard;

        v_flex()
            .w_full()
            .gap(px(16.0))
            .when(disabled, |page| {
                page.child(div().text_xs().text_color(muted).child("保存中…"))
            })
            .when(self.clipboard_service.settings_degraded(), |page| {
                page.child(
                    div()
                        .text_xs()
                        .text_color(gpui::red())
                        .child("设置读取异常，采集已自动暂停；重新保存任一设置可尝试修复。"),
                )
            })
            .when(
                settings.enabled
                    && matches!(self.clipboard_service.hotkey_state(), HotkeyState::Failed),
                |page| {
                    page.child(div().text_xs().text_color(gpui::red()).child(format!(
                        "全局热键 {} 注册失败：组合键可能被其它应用占用，可尝试切换备用热键。",
                        clipboard_hotkey(settings.alternate_hotkey)
                    )))
                },
            )
            .child(
                settings_card("功能开关", border).child(clipboard_toggle_row(
                    "settings-clip-enabled",
                    "启用剪贴板",
                    format!(
                        "显示工具入口、后台记录剪贴历史并注册全局热键 {}",
                        clipboard_hotkey(settings.alternate_hotkey)
                    ),
                    settings.enabled,
                    disabled,
                    cx.listener(|this, _: &bool, _, cx| {
                        let mut next = this.clipboard.clone();
                        next.enabled = !next.enabled;
                        this.save_clipboard(next, cx);
                    }),
                    muted,
                )),
            )
            .child(
                settings_card("采集与粘贴", border)
                    .child(clipboard_toggle_row(
                        "settings-clip-hotkey-alt",
                        "备用全局热键",
                        format!(
                            "抽屉热键改用 {}，避免与其它应用的“粘贴为纯文本”（{}）冲突",
                            clipboard_hotkey(true),
                            clipboard_hotkey(false)
                        ),
                        settings.alternate_hotkey,
                        disabled,
                        cx.listener(|this, _: &bool, _, cx| {
                            let mut next = this.clipboard.clone();
                            next.alternate_hotkey = !next.alternate_hotkey;
                            this.save_clipboard(next, cx);
                        }),
                        muted,
                    ))
                    .child(clipboard_toggle_row(
                        "settings-clip-images",
                        "采集图片",
                        "记录复制的图片（占用磁盘较多）".to_string(),
                        settings.capture_images,
                        disabled,
                        cx.listener(|this, _: &bool, _, cx| {
                            let mut next = this.clipboard.clone();
                            next.capture_images = !next.capture_images;
                            this.save_clipboard(next, cx);
                        }),
                        muted,
                    ))
                    .child(clipboard_toggle_row(
                        "settings-clip-auto-paste",
                        "自动粘贴",
                        auto_paste_description().to_string(),
                        settings.auto_paste,
                        disabled,
                        cx.listener(|this, _: &bool, _, cx| {
                            let mut next = this.clipboard.clone();
                            next.auto_paste = !next.auto_paste;
                            this.save_clipboard(next, cx);
                        }),
                        muted,
                    )),
            )
            .child(render_blacklist_card(settings, disabled, border, muted, cx))
            .into_any_element()
    }

    fn restore_clipboard_source(&mut self, source_id: &str, cx: &mut Context<Self>) {
        let next = settings_without_source(self.clipboard.clone(), source_id);
        self.save_clipboard(next, cx);
    }
}

#[allow(clippy::too_many_arguments)]
fn clipboard_toggle_row(
    id: &'static str,
    title: &'static str,
    description: String,
    checked: bool,
    disabled: bool,
    on_click: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    muted: gpui::Hsla,
) -> impl IntoElement {
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
                .child(
                    div()
                        .text_sm()
                        .when(disabled, |row| row.text_color(muted))
                        .child(title),
                )
                .child(div().text_xs().text_color(muted).child(description)),
        )
        .child(
            crate::clickable_switch(id)
                .flex_none()
                .checked(checked)
                .disabled(disabled)
                .on_click(on_click),
        )
}

fn render_blacklist_card(
    settings: ClipboardSettings,
    disabled: bool,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    cx: &mut Context<SettingsView>,
) -> gpui::Div {
    settings_card("不记录的应用", border)
        .child(div().text_xs().text_color(muted).child(format!(
            "已排除 {}/{} 个应用；可在剪贴板条目详情中继续添加。",
            settings.blacklist.len(),
            MAX_CLIPBOARD_BLACKLIST_ENTRIES
        )))
        .when(settings.blacklist.is_empty(), |card| {
            card.child(div().text_xs().text_color(muted).child("尚未排除任何应用"))
        })
        .children(
            settings
                .blacklist
                .into_iter()
                .enumerate()
                .map(|(index, source_id)| {
                    let source_id_for_click = source_id.clone();
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(muted)
                                .child(source_display_name(&source_id)),
                        )
                        .child(
                            crate::clickable_button(("settings-clip-restore-source", index))
                                .ghost()
                                .small()
                                .label("恢复")
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.restore_clipboard_source(&source_id_for_click, cx);
                                })),
                        )
                }),
        )
}

fn settings_without_source(mut settings: ClipboardSettings, source_id: &str) -> ClipboardSettings {
    settings.blacklist.retain(|id| id != source_id);
    settings
}

fn source_display_name(source_id: &str) -> String {
    source_id
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(source_id)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{settings_without_source, source_display_name};
    use ramag_domain::entities::ClipboardSettings;

    #[test]
    fn restoring_source_removes_only_the_selected_entry() {
        let settings = ClipboardSettings {
            blacklist: vec!["first.app".into(), "second.app".into()],
            ..ClipboardSettings::default()
        };

        let restored = settings_without_source(settings, "first.app");

        assert_eq!(restored.blacklist, vec!["second.app"]);
    }

    #[test]
    fn source_name_supports_both_platform_path_separators() {
        assert_eq!(source_display_name("/Applications/Ramag.app"), "Ramag.app");
        assert_eq!(source_display_name(r"C:\Apps\Ramag.exe"), "Ramag.exe");
    }
}
