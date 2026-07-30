//! 设置导航与各独立页面的渲染。

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window, div,
    hsla, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, Selectable as _, Sizable as _, h_flex, scroll::ScrollableElement as _,
    v_flex,
};

use super::{SettingsPage, SettingsView};
use crate::theme::{Mode, apply_theme, current_mode};

impl SettingsPage {
    fn icon(self) -> Icon {
        match self {
            Self::Global => crate::icons::settings(),
            Self::Database => crate::icons::database(),
            Self::VersionControl => crate::icons::git_branch(),
            Self::Clipboard => crate::icons::clipboard(),
            Self::Ssh => crate::activity_bar::ActivityBar::icon_for_tool("ssh"),
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

        for page in SettingsPage::ALL {
            let selected = self.selected_page == page;
            navigation = navigation.child(settings_navigation_item(
                page,
                selected,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.selected_page != page {
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
            SettingsPage::Global => self.render_global_page(cx),
            SettingsPage::Clipboard => self.render_clipboard_page(cx),
            SettingsPage::Database => managed_in_module_card(
                "连接配置",
                "数据库地址、认证、TLS 与 SSH 隧道等配置跟随连接保存，请在数据库客户端的连接管理中维护。",
                cx,
            ),
            SettingsPage::VersionControl => managed_in_module_card(
                "Git 配置",
                "Ramag 复用系统 Git、SSH Agent 与已有凭据；仓库和远程地址请在版本管理页面中维护。",
                cx,
            ),
            SettingsPage::Ssh => managed_in_module_card(
                "连接配置",
                "主机、认证方式、密钥与传输配置跟随 SSH 连接保存，请在 SSH 的连接管理中维护。",
                cx,
            ),
        };

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

    fn render_global_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = current_mode(cx);
        let muted = cx.theme().muted_foreground;
        settings_card("外观", cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("主题修改会立即应用到所有窗口。"),
            )
            .child(
                h_flex()
                    .gap(px(8.0))
                    .child(theme_button(
                        "settings-theme-light",
                        "浅色",
                        mode == Mode::Light,
                        Mode::Light,
                    ))
                    .child(theme_button(
                        "settings-theme-dark",
                        "深色",
                        mode == Mode::Dark,
                        Mode::Dark,
                    )),
            )
            .into_any_element()
    }
}

fn settings_navigation_item(
    page: SettingsPage,
    selected: bool,
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

fn theme_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    mode: Mode,
) -> impl IntoElement {
    crate::clickable_button(id)
        .small()
        .label(label)
        .selected(selected)
        .on_click(move |_: &ClickEvent, _, cx| {
            if current_mode(cx) == mode {
                return;
            }
            apply_theme(mode, cx);
            cx.refresh_windows();
            crate::preferences::persist_preference_latest(
                "theme_mode",
                match mode {
                    Mode::Dark => "dark",
                    Mode::Light => "light",
                }
                .to_string(),
                cx,
            );
        })
}

fn managed_in_module_card(
    title: &'static str,
    description: &'static str,
    cx: &Context<SettingsView>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    settings_card(title, cx.theme().border)
        .child(div().text_sm().text_color(muted).child(description))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("当前没有需要在此设置的模块级通用选项。"),
        )
        .into_any_element()
}
