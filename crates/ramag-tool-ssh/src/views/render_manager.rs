//! SSH 连接管理页；复用数据库连接页的限宽工具栏与紧凑行布局。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    h_flex, v_flex,
};
use ramag_domain::entities::{SshAuthMode, SshProfile, contains_case_insensitive};

use super::SshView;

const CONTENT_MAX_W: f32 = 1080.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowDensity {
    Full,
    Medium,
    Narrow,
}

impl SshView {
    pub(super) fn render_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !self.focused_search_once {
            self.focused_search_once = true;
            self.search.update(cx, |state, cx| state.focus(window, cx));
        }

        let width = f32::from(window.viewport_size().width);
        let density = if width < 900.0 {
            RowDensity::Narrow
        } else if width < 1120.0 {
            RowDensity::Medium
        } else {
            RowDensity::Full
        };
        let visible = self.filtered_profiles();
        let total = self.profiles.len();
        let visible_count = visible.len();
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;

        let header_inner = h_flex()
            .w_full()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .id("ssh-profile-search")
                    .debug_selector(|| "ssh-profile-search".into())
                    .flex_1()
                    .min_w_0()
                    .child(
                        div().max_w(px(360.0)).child(
                            ramag_ui::cleanable_input(
                                &self.search,
                                "ssh-profile-search-clear",
                                false,
                                cx,
                            )
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                        ),
                    ),
            )
            .child(
                div()
                    .id("import-jumpserver-profile")
                    .debug_selector(|| "import-jumpserver-profile".into())
                    .child(
                        ramag_ui::clickable_button("import-jumpserver-profile-button")
                            .outline()
                            .small()
                            .icon(ramag_ui::icons::download())
                            .tooltip("从 JumpServer 获取资源")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_jumpserver_assets(window, cx);
                            })),
                    ),
            )
            .child(
                ramag_ui::clickable_button("new-ssh-profile")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_profile_create(window, cx);
                    })),
            );

        let header = h_flex()
            .w_full()
            .justify_center()
            .px(px(24.0))
            .pt(px(22.0))
            .pb(px(16.0))
            .border_b_1()
            .border_color(border)
            .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(header_inner));

        let body = if self.loading_profiles {
            centered_message("加载中…", muted).into_any_element()
        } else if let Some(error) = &self.load_error {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
                .child(
                    ramag_ui::clickable_button("retry-ssh-profiles")
                        .small()
                        .label("重试")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.loading_profiles = true;
                            this.load_initial_state(window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else if total == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .id("empty-import-jumpserver-profile")
                                .debug_selector(|| "empty-import-jumpserver-profile".into())
                                .child(
                                    ramag_ui::clickable_button(
                                        "empty-import-jumpserver-profile-button",
                                    )
                                    .outline()
                                    .icon(ramag_ui::icons::download())
                                    .tooltip("从 JumpServer 获取资源")
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.open_jumpserver_assets(window, cx);
                                        },
                                    )),
                                ),
                        )
                        .child(
                            ramag_ui::clickable_button("empty-add-ssh-profile")
                                .primary()
                                .icon(IconName::Plus)
                                .label("新建")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.open_profile_create(window, cx);
                                })),
                        ),
                )
                .into_any_element()
        } else if visible_count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(div().text_sm().child("暂无匹配"))
                .into_any_element()
        } else {
            let mut rows = v_flex().w_full();
            for (index, profile) in visible.into_iter().enumerate() {
                rows = rows.child(self.profile_row(index, profile, density, cx));
            }
            div()
                .id("ssh-profile-list-scroll")
                .size_full()
                .overflow_y_scroll()
                .py(px(10.0))
                .child(
                    h_flex()
                        .w_full()
                        .justify_center()
                        .px(px(24.0))
                        .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(rows)),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }

    fn filtered_profiles(&self) -> Vec<SshProfile> {
        self.profiles
            .iter()
            .filter(|profile| profile_matches_query(profile, &self.query))
            .cloned()
            .collect()
    }

    fn profile_row(
        &self,
        index: usize,
        profile: SshProfile,
        density: RowDensity,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = profile.id.clone();
        let id_for_edit = id.clone();
        let id_for_delete = id.clone();
        let endpoint = profile.port.map_or_else(
            || profile.host.clone(),
            |port| format!("{}:{port}", profile.host),
        );
        let auth_label = match profile.auth_mode {
            SshAuthMode::System => "系统",
            SshAuthMode::Password => "密码",
            SshAuthMode::KeyFile => "密钥",
        };
        let username = profile.username.clone();
        let environment = profile.environment.clone().unwrap_or_default();
        let production = profile.production;
        let name = profile.name.clone();
        let selected = self.active_workspace_id.as_ref() == Some(&id);
        let connection_available = self.profile_connection_available(&profile);
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let danger = cx.theme().danger;
        let mut badge_bg = accent;
        badge_bg.a = 0.12;
        let mut production_bg = danger;
        production_bg.a = 0.12;

        h_flex()
            .id(SharedString::from(format!("ssh-profile-row-{index}-{id}")))
            .debug_selector(move || format!("ssh-profile-row-{index}"))
            .w_full()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(border)
            .cursor_pointer()
            .when(selected, |row| {
                let mut selected_bg = accent;
                selected_bg.a = 0.06;
                row.bg(selected_bg)
            })
            .when(!connection_available, |row| row.opacity(0.65))
            .hover(|row| row.bg(cx.theme().muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_workspace(id.clone(), window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(24.0))
                    .flex()
                    .justify_center()
                    .child(Icon::new(IconName::Network).small().text_color(muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(name),
            )
            .child(environment_badge(environment, muted))
            .child(
                div().flex_none().w(px(92.0)).flex().justify_center().child(
                    div()
                        .max_w_full()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .text_xs()
                        .text_color(accent)
                        .bg(badge_bg)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(auth_label),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(44.0))
                    .flex()
                    .justify_center()
                    .when(production, |slot| {
                        slot.child(
                            div()
                                .px(px(6.0))
                                .py(px(1.0))
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(danger)
                                .bg(production_bg)
                                .child("生产"),
                        )
                    }),
            )
            .when(density != RowDensity::Narrow, |row| {
                row.child(secondary_column(220.0, endpoint, muted))
            })
            .when(density == RowDensity::Full, |row| {
                row.child(secondary_column(150.0, username, muted))
            })
            .child(
                h_flex()
                    .flex_none()
                    .w(px(72.0))
                    .justify_end()
                    .gap(px(4.0))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "edit-ssh-profile-{id_for_edit}"
                        )))
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::pencil())
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_profile_edit(id_for_edit.clone(), window, cx);
                            },
                        )),
                    )
                    .child(
                        ramag_ui::clickable_button(SharedString::from(format!(
                            "delete-ssh-profile-{id_for_delete}"
                        )))
                        .ghost()
                        .small()
                        .icon(ramag_ui::icons::trash())
                        .disabled(self.deleting_profile)
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.request_delete_profile(id_for_delete.clone(), window, cx);
                            },
                        )),
                    ),
            )
    }
}

fn centered_message(message: &'static str, color: gpui::Hsla) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(div().text_sm().text_color(color).child(message))
}

fn profile_matches_query(profile: &SshProfile, query: &str) -> bool {
    contains_case_insensitive(&profile.name, query)
        || contains_case_insensitive(&profile.host, query)
        || contains_case_insensitive(&profile.username, query)
        || profile
            .environment
            .as_deref()
            .is_some_and(|environment| contains_case_insensitive(environment, query))
}

fn secondary_column(width: f32, text: String, color: gpui::Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(width))
        .text_xs()
        .text_color(color)
        .overflow_hidden()
        .text_ellipsis()
        .child(text)
}

fn environment_badge(environment: String, fallback: gpui::Hsla) -> impl IntoElement {
    let slot = div().flex_none().w(px(64.0)).flex().justify_center();
    if environment.trim().is_empty() {
        slot
    } else {
        let (foreground, background) = environment_badge_colors(&environment, fallback);
        slot.child(
            div()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(4.0))
                .text_xs()
                .text_color(foreground)
                .bg(background)
                .max_w_full()
                .overflow_hidden()
                .text_ellipsis()
                .child(environment),
        )
    }
}

pub(super) fn environment_badge_colors(
    environment: &str,
    fallback: gpui::Hsla,
) -> (gpui::Hsla, gpui::Hsla) {
    let foreground = match environment.trim().to_ascii_lowercase().as_str() {
        "dev" => gpui::hsla(140.0 / 360.0, 0.55, 0.42, 1.0),
        "test" => gpui::hsla(35.0 / 360.0, 0.80, 0.45, 1.0),
        "prod" => gpui::hsla(0.0, 0.70, 0.55, 1.0),
        _ => fallback,
    };
    let mut background = foreground;
    background.a = 0.12;
    (foreground, background)
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::SshProfile;

    use super::{environment_badge_colors, profile_matches_query};

    #[test]
    fn environment_presets_have_distinct_badge_colors() {
        let fallback = gpui::black();
        assert_ne!(
            environment_badge_colors("dev", fallback).0,
            environment_badge_colors("prod", fallback).0
        );
    }

    #[test]
    fn profile_search_matches_name_host_and_username() {
        let mut profile = SshProfile::new("Production", "SERVER.EXAMPLE");
        profile.username = "Alice".into();
        profile.environment = Some("staging".into());

        assert!(profile_matches_query(&profile, "production"));
        assert!(profile_matches_query(&profile, "server.example"));
        assert!(profile_matches_query(&profile, "alice"));
        assert!(profile_matches_query(&profile, "staging"));
        assert!(!profile_matches_query(&profile, "missing"));
    }
}
