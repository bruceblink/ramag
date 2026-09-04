use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
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
            .child(render_update_toolbar(
                format!("当前版本：{current_version}"),
                update,
                accent,
                link_hover,
            ))
            .into_any_element()
    }
}

/// 将版本信息和外链操作分成可收缩的布局组，保证更新提示和按钮在窄容器内换行。
fn render_update_toolbar(
    current_version: String,
    update: Option<AvailableUpdate>,
    accent: gpui::Hsla,
    link_hover: gpui::Hsla,
) -> impl IntoElement {
    let mut info = h_flex()
        .id("settings-update-info")
        .debug_selector(|| "settings-update-info".into())
        .flex_1()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .debug_selector(|| "settings-update-current-version".into())
                .flex_1()
                .min_w_0()
                .text_sm()
                .whitespace_normal()
                .child(current_version),
        );
    if let Some(update) = update {
        let release_url = update.release.release_url.clone();
        info = info.child(
            h_flex()
                .debug_selector(|| "settings-update-available".into())
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child("新"),
                )
                .child(
                    div()
                        .id("settings-update-release-page")
                        .debug_selector(|| "settings-update-release-page".into())
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .whitespace_normal()
                        .cursor_pointer()
                        .hover(move |link| link.text_color(link_hover))
                        .child(format!("新版本：{}", update.release.version))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            cx.open_url(&release_url);
                        }),
                ),
        );
    }

    h_flex()
        .id("settings-update-toolbar")
        .debug_selector(|| "settings-update-toolbar".into())
        .w_full()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
        .child(info)
        .child(
            h_flex()
                .id("settings-update-actions")
                .debug_selector(|| "settings-update-actions".into())
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .child(
                    crate::clickable_button("settings-community")
                        .debug_selector(|| "settings-community".into())
                        .outline()
                        .small()
                        .label("交流群")
                        .on_click(|_: &ClickEvent, _, cx| {
                            cx.open_url(crate::COMMUNITY_URL);
                        }),
                )
                .child(
                    crate::clickable_button("settings-feedback-issue")
                        .debug_selector(|| "settings-feedback-issue".into())
                        .outline()
                        .small()
                        .label("反馈问题")
                        .on_click(|_: &ClickEvent, _, cx| {
                            cx.open_url(crate::FEEDBACK_ISSUE_URL);
                        }),
                ),
        )
}

fn update_from_state(state: &UpdateUiState) -> Option<&AvailableUpdate> {
    match state {
        UpdateUiState::Available(update) | UpdateUiState::UnsupportedPlatform(update) => {
            Some(update)
        }
        UpdateUiState::Idle | UpdateUiState::UpToDate => None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, VisualTestContext, Window, div, px, size,
    };
    use gpui_component::ActiveTheme as _;
    use ramag_app::AvailableUpdate;
    use ramag_domain::entities::ReleaseInfo;

    use super::render_update_toolbar;

    struct UpdateToolbarHost;

    impl Render for UpdateToolbarHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = cx.theme();
            div()
                .id("settings-update-toolbar-host")
                .debug_selector(|| "settings-update-toolbar-host".into())
                .w(px(96.0))
                .h(px(600.0))
                .child(render_update_toolbar(
                    "当前版本：0.1.0-这是一个足够长的版本标识".into(),
                    Some(AvailableUpdate {
                        release: ReleaseInfo {
                            version: "0.2.0-这是一个足够长的更新版本标识".into(),
                            tag_name: "v0.2.0".into(),
                            release_url: "https://example.com/releases/v0.2.0".into(),
                            notes: String::new(),
                            published_at: None,
                            assets: Vec::new(),
                        },
                        asset: None,
                    }),
                    theme.accent,
                    theme.link_hover,
                ))
        }
    }

    #[gpui::test]
    fn update_toolbar_keeps_long_info_and_actions_inside_narrow_parent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| UpdateToolbarHost);
        let cx: &mut VisualTestContext = cx;
        cx.simulate_resize(size(px(240.0), px(180.0)));
        cx.run_until_parked();

        let Some(host) = cx.debug_bounds("settings-update-toolbar-host") else {
            unreachable!("更新工具栏宿主应渲染");
        };
        let Some(toolbar) = cx.debug_bounds("settings-update-toolbar") else {
            unreachable!("更新工具栏应渲染");
        };
        let Some(info) = cx.debug_bounds("settings-update-info") else {
            unreachable!("版本信息组应渲染");
        };
        let Some(actions) = cx.debug_bounds("settings-update-actions") else {
            unreachable!("操作组应渲染");
        };
        let Some(version) = cx.debug_bounds("settings-update-current-version") else {
            unreachable!("当前版本应渲染");
        };
        let Some(release) = cx.debug_bounds("settings-update-release-page") else {
            unreachable!("更新版本链接应渲染");
        };
        let Some(community) = cx.debug_bounds("settings-community") else {
            unreachable!("交流群按钮应渲染");
        };
        let Some(feedback) = cx.debug_bounds("settings-feedback-issue") else {
            unreachable!("反馈问题按钮应渲染");
        };

        for child in [
            toolbar, info, actions, version, release, community, feedback,
        ] {
            assert!(child.origin.x >= host.origin.x);
            assert!(child.origin.y >= host.origin.y);
            assert!(child.origin.x + child.size.width <= host.origin.x + host.size.width);
            assert!(child.origin.y + child.size.height <= host.origin.y + host.size.height);
            assert!(child.origin.x + child.size.width <= toolbar.origin.x + toolbar.size.width);
            assert!(child.origin.y + child.size.height <= toolbar.origin.y + toolbar.size.height);
        }
        assert!(feedback.origin.y > community.origin.y);
    }
}
