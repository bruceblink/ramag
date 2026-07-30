//! 设置中心：默认展示全局配置，各大模块使用独立页面。

mod clipboard;
mod database;
mod pages;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
};
use gpui_component::{WindowExt as _, h_flex, input::InputState, notification::Notification};
use ramag_app::ClipboardService;
use ramag_domain::entities::ClipboardSettings;

use crate::database_search::MAX_ID_CONVERTER_PROGRAM_BYTES;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    Global,
    Database,
    VersionControl,
    Clipboard,
    Ssh,
}

impl SettingsPage {
    const ALL: [Self; 5] = [
        Self::Global,
        Self::Database,
        Self::VersionControl,
        Self::Clipboard,
        Self::Ssh,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Database => "database",
            Self::VersionControl => "version-control",
            Self::Clipboard => "clipboard",
            Self::Ssh => "ssh",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Global => "全局配置",
            Self::Database => "数据库客户端",
            Self::VersionControl => "版本管理",
            Self::Clipboard => "剪贴板",
            Self::Ssh => "SSH",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Global => "管理对整个应用生效的设置。",
            Self::Database => "管理数据库客户端的模块级配置。",
            Self::VersionControl => "管理 Git 版本控制的模块级配置。",
            Self::Clipboard => "管理剪贴板的启用状态、采集行为、全局热键与排除应用。",
            Self::Ssh => "管理 SSH 与 SFTP 的模块级配置。",
        }
    }
}

pub struct SettingsView {
    selected_page: SettingsPage,
    clipboard_service: Arc<ClipboardService>,
    clipboard: ClipboardSettings,
    loaded_revision: u64,
    saving_clipboard: bool,
    database_enabled_draft: bool,
    database_converter_program: Entity<InputState>,
    saving_database: bool,
    picking_id_converter: bool,
    pending_notification: Option<Notification>,
}

impl SettingsView {
    pub fn new(
        clipboard_service: Arc<ClipboardService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (clipboard, loaded_revision) = clipboard_service.settings_snapshot_with_revision();
        let database = crate::database_search_settings(cx);
        let database_enabled_draft = database.id_conversion_enabled;
        let converter_program = database.id_converter_program.clone();
        let database_converter_program = cx.new(|cx| {
            InputState::new(window, cx)
                .validate(|value, _| value.len() <= MAX_ID_CONVERTER_PROGRAM_BYTES)
                .placeholder("请选择转换程序的绝对路径")
                .default_value(converter_program)
        });

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
            selected_page: SettingsPage::default(),
            clipboard_service,
            clipboard,
            loaded_revision,
            saving_clipboard: false,
            database_enabled_draft,
            database_converter_program,
            saving_database: false,
            picking_id_converter: false,
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
        h_flex()
            .size_full()
            .min_w_0()
            .child(self.render_navigation(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_selected_page(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::SettingsPage;

    #[test]
    fn global_configuration_is_the_default_page() {
        assert_eq!(SettingsPage::default(), SettingsPage::Global);
        assert_eq!(SettingsPage::ALL.first(), Some(&SettingsPage::Global));
    }

    #[test]
    fn each_large_module_has_a_distinct_page() {
        let ids: HashSet<_> = SettingsPage::ALL
            .into_iter()
            .map(SettingsPage::id)
            .collect();

        assert_eq!(ids.len(), SettingsPage::ALL.len());
        assert!(ids.contains("database"));
        assert!(ids.contains("version-control"));
        assert!(ids.contains("clipboard"));
        assert!(ids.contains("ssh"));
    }
}
