use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, hsla, prelude::*, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};

use super::{SETTINGS_COMPACT_NAV_ITEM_WIDTH, SettingsPage, SettingsView, settings_is_compact};

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
    pub(super) fn render_navigation(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let compact = settings_is_compact(window);
        let theme = cx.theme();
        let style = SettingsNavigationStyle {
            active: theme.list_active,
            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            hover: theme.list_hover,
            accent: theme.accent,
        };
        let mut children = Vec::new();
        let update_available =
            cx.read_global::<crate::activity_bar::UpdateIndicatorGlobal, _>(|state, _| {
                state.available
            });

        for &page in SettingsPage::ALL
            .iter()
            .filter(|&&page| page != SettingsPage::Clipboard || self.clipboard_service.is_some())
        {
            let selected = self.selected_page == page;
            children.push(
                settings_navigation_item(
                    page,
                    selected,
                    update_available && page == SettingsPage::Update,
                    compact,
                    style,
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
                )
                .into_any_element(),
            );
        }
        settings_navigation_shell(compact, theme.sidebar, theme.border, children)
    }

    pub(super) fn render_selected_page(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let compact = settings_is_compact(window);
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
                    .p(px(if compact { 16.0 } else { 28.0 }))
                    .gap(px(if compact { 16.0 } else { 24.0 }))
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

#[derive(Clone, Copy)]
struct SettingsNavigationStyle {
    active: gpui::Hsla,
    foreground: gpui::Hsla,
    muted_foreground: gpui::Hsla,
    hover: gpui::Hsla,
    accent: gpui::Hsla,
}

fn settings_navigation_shell(
    compact: bool,
    sidebar: gpui::Hsla,
    border: gpui::Hsla,
    children: Vec<AnyElement>,
) -> impl IntoElement {
    let title = div()
        .id("settings-navigation-title")
        .debug_selector(|| "settings-navigation-title".into())
        .when(compact, |title| title.w(px(56.0)).flex_none())
        .when(!compact, |title| {
            title.px(px(10.0)).pt(px(6.0)).pb(px(14.0))
        })
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child("设置");
    if compact {
        h_flex()
            .id("settings-navigation")
            .debug_selector(|| "settings-navigation".into())
            .w_full()
            .h(px(66.0))
            .flex_none()
            .items_start()
            .gap(px(4.0))
            .overflow_x_scroll()
            .px(px(8.0))
            .py(px(8.0))
            .bg(sidebar)
            .border_b_1()
            .border_color(border)
            .child(title)
            .children(children)
    } else {
        v_flex()
            .id("settings-navigation")
            .debug_selector(|| "settings-navigation".into())
            .w(px(220.0))
            .h_full()
            .flex_none()
            .p(px(16.0))
            .gap(px(4.0))
            .bg(sidebar)
            .border_r_1()
            .border_color(border)
            .child(title)
            .children(children)
    }
}

fn settings_navigation_item(
    page: SettingsPage,
    selected: bool,
    show_update_badge: bool,
    compact: bool,
    style: SettingsNavigationStyle,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let debug_selector = format!("settings-page-{}", page.id());
    let background = if selected {
        style.active
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };
    let foreground = if selected {
        style.foreground
    } else {
        style.muted_foreground
    };

    h_flex()
        .id(SharedString::from(format!("settings-page-{}", page.id())))
        .debug_selector(move || debug_selector.clone())
        .w_full()
        .h(px(38.0))
        .when(compact, |item| {
            item.w(px(SETTINGS_COMPACT_NAV_ITEM_WIDTH)).flex_none()
        })
        .px(px(10.0))
        .gap(px(9.0))
        .items_center()
        .rounded(px(6.0))
        .bg(background)
        .text_color(foreground)
        .cursor_pointer()
        .hover(move |item| item.bg(style.hover))
        .on_click(on_click)
        .child(page.icon().small())
        .child(div().text_sm().child(page.title()))
        .when(show_update_badge, |item| {
            item.child(
                div()
                    .ml_auto()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(style.accent)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use gpui::{Render, TestAppContext, size};

    struct SettingsNavigationTestHost;

    impl Render for SettingsNavigationTestHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let compact = settings_is_compact(window);
            let theme = cx.theme();
            let style = SettingsNavigationStyle {
                active: theme.list_active,
                foreground: theme.foreground,
                muted_foreground: theme.muted_foreground,
                hover: theme.list_hover,
                accent: theme.accent,
            };
            let children = SettingsPage::ALL
                .into_iter()
                .map(|page| {
                    settings_navigation_item(
                        page,
                        page == SettingsPage::System,
                        false,
                        compact,
                        style,
                        |_, _, _| {},
                    )
                    .into_any_element()
                })
                .collect();
            super::super::render_settings_layout(
                compact,
                settings_navigation_shell(compact, theme.sidebar, theme.border, children),
                div()
                    .id("settings-test-page")
                    .debug_selector(|| "settings-test-page".into())
                    .size_full(),
            )
        }
    }

    #[gpui::test]
    fn settings_navigation_switches_to_scrollable_strip_on_compact_widths(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual_cx) = cx.add_window_view(|_, _| SettingsNavigationTestHost);

        for (width, height) in [(360.0, 520.0), (1024.0, 520.0), (1440.0, 520.0)] {
            visual_cx.simulate_resize(size(px(width), px(height)));
            visual_cx.run_until_parked();

            let navigation = visual_cx
                .debug_bounds("settings-navigation")
                .expect("设置导航应参与布局");
            let root = visual_cx
                .debug_bounds("settings-root")
                .expect("设置根布局应参与布局");
            let content = visual_cx
                .debug_bounds("settings-content")
                .expect("设置内容区应参与布局");
            let title = visual_cx
                .debug_bounds("settings-navigation-title")
                .expect("设置导航标题应渲染");
            let system = visual_cx
                .debug_bounds("settings-page-system")
                .expect("系统设置入口应渲染");
            let database = visual_cx
                .debug_bounds("settings-page-database")
                .expect("数据库入口应渲染");
            let update = visual_cx
                .debug_bounds("settings-page-update")
                .expect("关于入口应渲染");

            assert!(navigation.origin.x >= px(0.0));
            assert!(navigation.right() <= px(width));
            assert!(root.origin.x >= px(0.0));
            assert!(root.right() <= px(width));
            assert!(root.bottom() <= px(height));
            assert!(content.origin.x >= root.origin.x);
            assert!(content.right() <= root.right());
            assert!(content.bottom() <= root.bottom());
            assert!(title.origin.x >= navigation.origin.x);
            assert!(system.origin.x >= navigation.origin.x);
            assert!(system.origin.y >= navigation.origin.y);
            if width < 900.0 {
                assert!(navigation.size.height <= px(66.0));
                assert!(content.origin.y >= navigation.bottom());
                assert!(system.size.width >= px(SETTINGS_COMPACT_NAV_ITEM_WIDTH));
                assert!(database.size.width >= px(SETTINGS_COMPACT_NAV_ITEM_WIDTH));
                assert!(system.right() <= navigation.right());
                assert!(system.origin.x > title.origin.x);
            } else {
                assert!(navigation.size.width <= px(220.0));
                assert!(content.origin.x >= navigation.right());
                assert!(system.right() <= navigation.right());
                assert!(update.right() <= navigation.right());
                assert!(update.origin.y > system.origin.y);
            }
        }
    }
}
