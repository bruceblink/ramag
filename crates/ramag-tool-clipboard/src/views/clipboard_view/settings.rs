//! 设置面板：采集开关 / 图片采集 / 自动粘贴 / 清空

use gpui::{ClickEvent, Context, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    switch::Switch,
    v_flex,
};
use ramag_ui::{
    open_confirm,
    platform::{auto_paste_description, clipboard_hotkey},
};

use super::ClipboardView;

impl ClipboardView {
    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let s = self.settings.clone();

        v_flex()
            .id("clip-settings-scroll")
            .size_full()
            .p(px(16.0))
            .gap(px(14.0))
            .overflow_y_scroll()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("剪贴板设置"),
            )
            // 设置降级（读取失败 / 损坏）：采集已 fail-closed 暂停，必须让用户知情
            .when(self.service.settings_degraded(), |view| {
                view.child(div().text_xs().text_color(gpui::red()).child(
                    "设置读取异常，采集已自动暂停（不会记录新内容）；重新保存任一设置可尝试修复",
                ))
            })
            // 启用采集为总开关，关闭后下面几项均失效（变灰不可点）
            .child(self.toggle_row(
                "clip-enabled",
                "启用采集",
                &format!(
                    "关闭后停止记录新内容，并释放全局快捷键 {}",
                    clipboard_hotkey(s.alternate_hotkey)
                ),
                s.enabled,
                false,
                muted,
                cx.listener(|this, _: &bool, _, cx| {
                    let mut next = this.settings.clone();
                    next.enabled = !next.enabled;
                    this.save_settings(next, cx);
                }),
            ))
            // 热键注册失败对用户可见（常见原因：组合键被其它应用占用），成功时不打扰
            .when(
                s.enabled && matches!(self.service.hotkey_state(), ramag_app::HotkeyState::Failed),
                |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(gpui::red())
                            .child(format!(
                                "全局热键 {} 注册失败：组合键可能被其它应用占用，可尝试切换「备用全局热键」",
                                clipboard_hotkey(s.alternate_hotkey)
                            )),
                    )
                },
            )
            .child(self.toggle_row(
                "clip-hotkey-alt",
                "备用全局热键",
                &format!(
                    "抽屉热键改用 {}，避免与其它应用的「粘贴为纯文本」（{}）冲突",
                    clipboard_hotkey(true),
                    clipboard_hotkey(false)
                ),
                s.alternate_hotkey,
                !s.enabled,
                muted,
                cx.listener(|this, _: &bool, _, cx| {
                    let mut next = this.settings.clone();
                    next.alternate_hotkey = !next.alternate_hotkey;
                    this.save_settings(next, cx);
                }),
            ))
            .child(self.toggle_row(
                "clip-images",
                "采集图片",
                "记录复制的图片（占用磁盘较多）",
                s.capture_images,
                !s.enabled,
                muted,
                cx.listener(|this, _: &bool, _, cx| {
                    let mut next = this.settings.clone();
                    next.capture_images = !next.capture_images;
                    this.save_settings(next, cx);
                }),
            ))
            .child(self.toggle_row(
                "clip-autopaste",
                "自动粘贴",
                auto_paste_description(),
                s.auto_paste,
                !s.enabled,
                muted,
                cx.listener(|this, _: &bool, _, cx| {
                    let mut next = this.settings.clone();
                    next.auto_paste = !next.auto_paste;
                    this.save_settings(next, cx);
                }),
            ))
            .child(div().h(px(1.0)).bg(border))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("不记录的应用"),
            )
            .when(s.blacklist.is_empty(), |view| {
                view.child(div().text_xs().text_color(muted).child("尚未排除任何应用"))
            })
            .children(s.blacklist.iter().enumerate().map(|(index, source_id)| {
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
                            .child(source_display_name(source_id)),
                    )
                    .child(
                        Button::new(("clip-unblock-source", index))
                            .ghost()
                            .small()
                            .label("恢复记录")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.unblacklist_source(&source_id_for_click, cx);
                            })),
                    )
            }))
            .child(div().h(px(1.0)).bg(border))
            // 清空历史（移入设置，避免顶栏误触）
            .child(
                h_flex().w_full().items_center().justify_between().child(
                    Button::new("clip-clear-all")
                        .danger()
                        .small()
                        .label("清空历史")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.confirm_clear(window, cx);
                        })),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("历史全本地存储并加密，不会上传任何服务器"),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(
        &self,
        id: &'static str,
        title: &str,
        desc: &str,
        checked: bool,
        disabled: bool,
        muted: gpui::Hsla,
        on_click: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                v_flex()
                    .gap(px(2.0))
                    // 禁用时标题随之弱化，与变灰的开关呼应
                    .child(
                        div()
                            .text_sm()
                            .when(disabled, |d| d.text_color(muted))
                            .child(title.to_string()),
                    )
                    .child(div().text_xs().text_color(muted).child(desc.to_string())),
            )
            .child(
                Switch::new(id)
                    .checked(checked)
                    .disabled(disabled)
                    .on_click(on_click),
            )
    }

    /// 清空历史二次确认（复用 ramag-ui 通用确认弹窗）
    pub(super) fn confirm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().clone();
        open_confirm(
            "清空剪贴历史",
            "将删除全部历史条目。此操作不可撤销。",
            "清空",
            true,
            move |_window, cx| {
                entity.update(cx, |this, cx| this.clear_all(cx));
            },
            window,
            cx,
        );
    }
}

fn source_display_name(source_id: &str) -> String {
    std::path::Path::new(source_id)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| source_id.to_string())
}
