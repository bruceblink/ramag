//! 全局设置中心：外观、SQL 安全限制与剪贴板常用设置。

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    ClickEvent, Context, EventEmitter, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    notification::Notification, scroll::ScrollableElement as _, v_flex,
};
use ramag_app::{ClipboardService, HotkeyState};
use ramag_domain::entities::ClipboardSettings;

use crate::platform::{auto_paste_description, clipboard_hotkey};
use crate::preferences::{
    SQL_AUTO_LIMIT_CHOICES, persist_preference_latest, set_sql_auto_limit, sql_auto_limit,
};
use crate::theme::{Mode, apply_theme, current_mode, is_following_system, set_following_system};

#[derive(Debug, Clone)]
pub enum SettingsEvent {
    OpenClipboardDetails,
}

pub struct SettingsView {
    clipboard_service: Arc<ClipboardService>,
    clipboard: ClipboardSettings,
    loaded_revision: u64,
    saving_clipboard: bool,
    pending_notification: Option<Notification>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

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

    #[allow(clippy::too_many_arguments)]
    fn toggle_row(
        &self,
        id: &'static str,
        title: &'static str,
        description: String,
        checked: bool,
        disabled: bool,
        on_click: impl Fn(&bool, &mut Window, &mut gpui::App) + 'static,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
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
                    .child(div().text_sm().child(title))
                    .child(div().text_xs().text_color(muted).child(description)),
            )
            .child(
                crate::clickable_switch(id)
                    .checked(checked)
                    .disabled(disabled)
                    .on_click(on_click),
            )
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
        let following_system = is_following_system(cx);
        let mode = current_mode(cx);
        let sql_limit = sql_auto_limit(cx);
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
                        settings_card("外观", border)
                            .child(div().text_xs().text_color(muted).child("主题修改会立即应用到所有窗口。"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(theme_button("theme-system", "跟随系统", following_system, |_, window, app| {
                                        apply_theme(crate::theme::mode_from_appearance(window.appearance()), app);
                                        set_following_system(app, true);
                                        persist_theme("system", app);
                                    }))
                                    .child(theme_button("theme-light", "浅色", !following_system && mode == Mode::Light, |_, _, app| {
                                        apply_theme(Mode::Light, app);
                                        set_following_system(app, false);
                                        persist_theme("light", app);
                                    }))
                                    .child(theme_button("theme-dark", "深色", !following_system && mode == Mode::Dark, |_, _, app| {
                                        apply_theme(Mode::Dark, app);
                                        set_following_system(app, false);
                                        persist_theme("dark", app);
                                    })),
                            ),
                    )
                    .child(
                        settings_card("数据库查询", border)
                            .child(div().text_xs().text_color(muted).child("未手写 LIMIT 的 SELECT 会自动补上限；修改后所有现有与新建 SQL 标签立即生效。"))
                            .child({
                                let mut row = h_flex().gap_2().flex_wrap();
                                for limit in SQL_AUTO_LIMIT_CHOICES {
                                    row = row.child(
                                        crate::clickable_button(("sql-limit", limit))
                                            .small()
                                            .label(format_limit(limit))
                                            .selected(sql_limit == Some(limit))
                                            .on_click(move |_: &ClickEvent, _, app| set_sql_auto_limit(Some(limit), app)),
                                    );
                                }
                                row.child(
                                    crate::clickable_button("sql-limit-off")
                                        .small()
                                        .label("关闭（不推荐）")
                                        .selected(sql_limit.is_none())
                                        .on_click(|_: &ClickEvent, _, app| set_sql_auto_limit(None, app)),
                                )
                            }),
                    )
                    .child(
                        settings_card("剪贴板", border)
                            .when(self.clipboard_service.settings_degraded(), |card| {
                                card.child(div().text_xs().text_color(gpui::red()).child("设置读取异常，采集已自动暂停；保存任一设置可尝试修复。"))
                            })
                            .when(
                                clipboard.enabled && matches!(self.clipboard_service.hotkey_state(), HotkeyState::Failed),
                                |card| card.child(div().text_xs().text_color(gpui::red()).child(format!(
                                    "全局热键 {} 注册失败，组合键可能被其它应用占用。",
                                    clipboard_hotkey(clipboard.alternate_hotkey)
                                ))),
                            )
                            .child(self.toggle_row(
                                "settings-clip-enabled",
                                "启用采集",
                                format!("关闭后停止记录并释放全局热键 {}", clipboard_hotkey(clipboard.alternate_hotkey)),
                                clipboard.enabled,
                                clipboard_disabled,
                                cx.listener(|this, _: &bool, _, cx| {
                                    let mut next = this.clipboard.clone();
                                    next.enabled = !next.enabled;
                                    this.save_clipboard(next, cx);
                                }),
                                cx,
                            ))
                            .child(self.toggle_row(
                                "settings-clip-images",
                                "采集图片",
                                "记录复制的图片（占用磁盘较多）".to_string(),
                                clipboard.capture_images,
                                clipboard_disabled || !clipboard.enabled,
                                cx.listener(|this, _: &bool, _, cx| {
                                    let mut next = this.clipboard.clone();
                                    next.capture_images = !next.capture_images;
                                    this.save_clipboard(next, cx);
                                }),
                                cx,
                            ))
                            .child(self.toggle_row(
                                "settings-clip-auto-paste",
                                "自动粘贴",
                                auto_paste_description().to_string(),
                                clipboard.auto_paste,
                                clipboard_disabled || !clipboard.enabled,
                                cx.listener(|this, _: &bool, _, cx| {
                                    let mut next = this.clipboard.clone();
                                    next.auto_paste = !next.auto_paste;
                                    this.save_clipboard(next, cx);
                                }),
                                cx,
                            ))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        crate::clickable_button("open-clipboard-settings")
                                            .small()
                                            .label("管理热键、排除应用与历史")
                                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                                cx.emit(SettingsEvent::OpenClipboardDetails);
                                            })),
                                    )
                                    .when(self.saving_clipboard, |row| row.child(div().text_xs().text_color(muted).child("保存中…"))),
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

fn theme_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    crate::clickable_button(id)
        .small()
        .label(label)
        .selected(selected)
        .on_click(on_click)
}

fn persist_theme(value: &'static str, app: &mut gpui::App) {
    app.refresh_windows();
    persist_preference_latest("theme_mode", value.to_string(), app);
}

fn format_limit(limit: usize) -> String {
    match limit {
        1_000 => "1,000 行".to_string(),
        10_000 => "10,000 行".to_string(),
        50_000 => "50,000 行".to_string(),
        value => format!("{value} 行"),
    }
}
