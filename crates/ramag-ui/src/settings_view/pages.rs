use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, hsla, prelude::*, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};

use super::{SettingsPage, SettingsView};

impl SettingsPage {
    fn icon(self) -> Icon {
        match self {
            Self::System => crate::icons::settings(),
            Self::Database => crate::icons::database(),
            Self::VersionControl => crate::icons::git_branch(),
            Self::Ssh => crate::activity_bar::ActivityBar::icon_for_tool("ssh"),
            Self::ObjectStorage => {
                crate::activity_bar::ActivityBar::icon_for_tool("object-storage")
            }
            Self::Update => Icon::new(IconName::Info),
            Self::Clipboard => crate::icons::clipboard(),
        }
    }
}

impl SettingsView {
    pub(super) fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut navigation = v_flex()
            .w(px(220.0))
            .h_full()
            .flex_none()
            .p(px(16.0))
            .gap(px(4.0))
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px(px(10.0))
                    .pt(px(6.0))
                    .pb(px(14.0))
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("设置"),
            );
        let update_available =
            cx.read_global::<crate::activity_bar::UpdateIndicatorGlobal, _>(|state, _| {
                state.available
            });

        for &page in SettingsPage::ALL
            .iter()
            .filter(|&&page| page != SettingsPage::Clipboard || self.clipboard_service.is_some())
        {
            let selected = self.selected_page == page;
            navigation = navigation.child(settings_navigation_item(
                page,
                selected,
                update_available && page == SettingsPage::Update,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if this.selected_page != page {
                        if this
                            .selected_page
                            .clears_database_test_when_switching_to(page)
                        {
                            this.clear_database_converter_test(window, cx);
                        }
                        this.selected_page = page;
                        cx.notify();
                    }
                }),
                cx,
            ));
        }
        navigation
    }

    pub(super) fn render_selected_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let page = self.selected_page;
        let content = match page {
            SettingsPage::System => self.render_system_page(cx),
            SettingsPage::Database => self.render_database_page(cx),
            SettingsPage::VersionControl => managed_in_module_card("Git 配置", cx),
            SettingsPage::Ssh => self.render_ssh_page(cx),
            SettingsPage::ObjectStorage => managed_in_module_card("账号与 Bucket", cx),
            SettingsPage::Update => self.render_update_page(cx),
            SettingsPage::Clipboard => self.render_clipboard_page(cx),
        };

        v_flex()
            .size_full()
            .id("settings-page-scroll")
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(820.0))
                    .mx_auto()
                    .p(px(28.0))
                    .gap(px(24.0))
                    .child(
                        v_flex()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(page.title()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(page.description()),
                            ),
                    )
                    .child(content),
            )
            .into_any_element()
    }
}

fn settings_navigation_item(
    page: SettingsPage,
    selected: bool,
    show_update_badge: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &Context<SettingsView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let background = if selected {
        theme.list_active
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };
    let foreground = if selected {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    let hover = theme.list_hover;

    h_flex()
        .id(SharedString::from(format!("settings-page-{}", page.id())))
        .w_full()
        .h(px(38.0))
        .px(px(10.0))
        .gap(px(9.0))
        .items_center()
        .rounded(px(6.0))
        .bg(background)
        .text_color(foreground)
        .cursor_pointer()
        .hover(move |item| item.bg(hover))
        .on_click(on_click)
        .child(page.icon().small())
        .child(div().text_sm().child(page.title()))
        .when(show_update_badge, |item| {
            item.child(
                div()
                    .ml_auto()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.accent)
                    .child("新"),
            )
        })
}

pub(super) fn settings_card(title: &'static str, border: gpui::Hsla) -> gpui::Div {
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

fn managed_in_module_card(title: &'static str, cx: &Context<SettingsView>) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    settings_card(title, cx.theme().border)
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("此模块暂无通用设置。"),
        )
        .into_any_element()
}
