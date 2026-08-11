use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
};

use super::model::{AccountSessionState, ObjectStorageView};

impl ObjectStorageView {
    pub(super) fn render_sessions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let accent = theme.accent;
        let muted_bg = theme.muted;
        let manager_selected = self.management_visible;
        let mut manager_tab = h_flex()
            .id("object-account-management")
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py(px(7.0))
            .border_r_1()
            .border_color(border)
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_account_management(cx);
            }))
            .child(
                Icon::new(IconName::HardDrive)
                    .small()
                    .text_color(if manager_selected { foreground } else { muted }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if manager_selected { foreground } else { muted })
                    .child("云存储"),
            );
        if manager_selected {
            let mut active_bg = accent;
            active_bg.a = 0.15;
            manager_tab = manager_tab.bg(active_bg);
        } else {
            manager_tab = manager_tab.hover(move |tab| tab.bg(muted_bg));
        }

        let mut sessions = h_flex()
            .id("object-workspace-tabs-scroll")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll();
        for id in &self.open_account_ids {
            let Some(account) = self.accounts.iter().find(|account| &account.id == id) else {
                continue;
            };
            let selected = !manager_selected && self.selected_account_id.as_ref() == Some(id);
            let account_id = id.clone();
            let close_id = id.clone();
            let dot_color = match self.account_session_states.get(id) {
                Some(AccountSessionState::Loading) => gpui::hsla(45.0 / 360.0, 0.9, 0.55, 1.0),
                Some(AccountSessionState::Configured) => theme.success,
                Some(AccountSessionState::Unverified) => theme.warning,
                None => muted,
            };
            let mut tab = h_flex()
                .id(SharedString::from(format!("object-session-{id}")))
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(7.0))
                .border_r_1()
                .border_color(border)
                .cursor_pointer()
                .child(div().size(px(8.0)).rounded_full().bg(dot_color))
                .child(
                    div()
                        .text_xs()
                        .text_color(if selected { foreground } else { muted })
                        .child(account.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(account.provider.display_name()),
                )
                .when(account.read_only, |tab| {
                    let mut chip_bg = theme.danger;
                    chip_bg.a = 0.15;
                    tab.child(
                        div()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .bg(chip_bg)
                            .text_xs()
                            .text_color(theme.danger)
                            .child(ramag_ui::PRODUCTION_BADGE_LABEL),
                    )
                })
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "close-object-session-{id}"
                    )))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.close_session(close_id.clone(), window, cx);
                        },
                    )),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.select_account(account_id.clone(), window, cx);
                }));
            if selected {
                let mut active_bg = accent;
                active_bg.a = 0.15;
                tab = tab.bg(active_bg);
            } else {
                tab = tab.hover(move |tab| tab.bg(muted_bg));
            }
            sessions = sessions.child(tab);
        }

        h_flex()
            .id("object-workspace-tabs")
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .bg(theme.secondary)
            .child(manager_tab)
            .child(sessions)
    }
}
