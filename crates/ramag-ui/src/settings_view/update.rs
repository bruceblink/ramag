use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{ActiveTheme, Sizable as _, h_flex};
use ramag_app::{AvailableUpdate, UpdateCheckResult};

use super::{SettingsView, UpdateUiState, pages::settings_card};

impl SettingsView {
    pub(super) fn sync_update_state(&mut self) {
        if !matches!(self.update_state, UpdateUiState::Idle) {
            return;
        }
        let Some(result) = self
            .update_service
            .as_ref()
            .and_then(|service| service.last_result())
        else {
            return;
        };
        self.update_state = match result {
            UpdateCheckResult::UpToDate { .. } => UpdateUiState::UpToDate,
            UpdateCheckResult::Available(update) => UpdateUiState::Available(update),
            UpdateCheckResult::UnsupportedPlatform(update) => {
                UpdateUiState::UnsupportedPlatform(update)
            }
        };
    }

    pub(super) fn render_update_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let accent = theme.accent;
        let link_hover = theme.link_hover;
        let current_version = self
            .update_service
            .as_ref()
            .map_or(env!("CARGO_PKG_VERSION"), |service| {
                service.current_version()
            });
        let update = update_from_state(&self.update_state).cloned();

        settings_card("版本信息", cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(20.0))
                    .child(
                        div()
                            .text_sm()
                            .child(format!("当前版本：{current_version}")),
                    )
                    .when_some(update, |row, update| {
                        let release_url = update.release.release_url.clone();
                        row.child(
                            div()
                                .id("settings-update-release-page")
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(accent)
                                .cursor_pointer()
                                .hover(move |link| link.text_color(link_hover))
                                .child(format!("新版本：{}", update.release.version))
                                .on_click(move |_: &ClickEvent, _, cx| {
                                    cx.open_url(&release_url);
                                }),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        crate::clickable_button("settings-feedback-issue")
                            .outline()
                            .small()
                            .label("反馈问题")
                            .on_click(|_: &ClickEvent, _, cx| {
                                cx.open_url(crate::FEEDBACK_ISSUE_URL);
                            }),
                    ),
            )
            .into_any_element()
    }
}

fn update_from_state(state: &UpdateUiState) -> Option<&AvailableUpdate> {
    match state {
        UpdateUiState::Available(update) | UpdateUiState::UnsupportedPlatform(update) => {
            Some(update)
        }
        UpdateUiState::Idle | UpdateUiState::UpToDate => None,
    }
}
