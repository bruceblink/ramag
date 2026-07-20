//! 左侧 Activity Bar：纯图标导航。顶部 Home 图标 + 每个工具图标，选中发 NavEvent

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, hsla, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, button::ButtonVariants as _, h_flex, v_flex};
use ramag_app::ToolRegistry;

use crate::icons;

#[derive(Debug, Clone, PartialEq)]
pub enum NavTarget {
    Home,
    Tool(String),
    Settings,
}

#[derive(Debug, Clone)]
pub enum NavEvent {
    Navigate(NavTarget),
}

const BAR_WIDTH: f32 = 48.0;
const ITEM_HEIGHT: f32 = 40.0;

pub struct ActivityBar {
    registry: Arc<ToolRegistry>,
    selected: NavTarget,
}

impl EventEmitter<NavEvent> for ActivityBar {}

impl ActivityBar {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            selected: NavTarget::Home,
        }
    }

    pub fn set_selected(&mut self, target: NavTarget, cx: &mut Context<Self>) {
        if self.selected != target {
            self.selected = target;
            cx.notify();
        }
    }

    fn navigate(&mut self, target: NavTarget, cx: &mut Context<Self>) {
        self.selected = target.clone();
        cx.emit(NavEvent::Navigate(target));
        cx.notify();
    }

    /// MySQL/Redis/Postgres 共用 dbclient 入口，driver 在连接表单内选。
    /// 首页工具卡片复用同一映射，保证入口图标一致
    pub(crate) fn icon_for_tool(tool_id: &str) -> Icon {
        match tool_id {
            "dbclient" => icons::database(),
            "vcs" => icons::git_branch(),
            "clipboard" => icons::clipboard(),
            "jsonfmt" => Icon::new(IconName::File),
            "url" => Icon::new(IconName::Globe),
            "hash" => Icon::new(IconName::MemoryStick),
            _ => Icon::new(IconName::Inbox),
        }
    }
}

impl Render for ActivityBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tools = self.registry.list();
        let selected = self.selected.clone();

        let accent = theme.accent;
        let sidebar_bg = theme.sidebar;
        let border = theme.border;
        let transparent = hsla(0.0, 0.0, 0.0, 0.0);

        let mut container = v_flex()
            .w(px(BAR_WIDTH))
            .h_full()
            .flex_none()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .py_2()
            .gap_1()
            .items_center();

        let is_home_selected = matches!(selected, NavTarget::Home);
        container = container.child(activity_item(
            "home",
            icons::home(),
            is_home_selected,
            accent,
            transparent,
            Some(SharedString::from("首页")),
            cx.listener(|this, _: &ClickEvent, _, cx| {
                this.navigate(NavTarget::Home, cx);
            }),
        ));

        container = container.child(div().w(px(20.0)).h(px(1.0)).bg(border).my_1());

        for tool in tools.iter() {
            let id = tool.meta().id.clone();
            let id_for_click = id.clone();
            let is_selected = matches!(&selected, NavTarget::Tool(s) if s == &id);
            let icon = Self::icon_for_tool(&id);
            let tip = SharedString::from(tool.meta().name.clone());

            container = container.child(activity_item(
                &format!("tool-{id}"),
                icon,
                is_selected,
                accent,
                transparent,
                Some(tip),
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.navigate(NavTarget::Tool(id_for_click.clone()), cx);
                }),
            ));
        }

        // 底部：主题快捷切换 + 完整设置中心。
        container = container.child(div().flex_1());
        let (theme_icon, theme_tip) =
            if matches!(crate::theme::current_mode(cx), crate::theme::Mode::Light) {
                (IconName::Sun, "外观：浅色 · 点击切为 深色")
            } else {
                (IconName::Moon, "外观：深色 · 点击切为 浅色")
            };
        container = container.child(activity_item(
            "theme-toggle",
            Icon::new(theme_icon),
            false,
            accent,
            transparent,
            Some(SharedString::from(theme_tip)),
            |_: &ClickEvent, _, app| {
                // 两态互切：浅色 ↔ 深色
                let next = match crate::theme::current_mode(app) {
                    crate::theme::Mode::Light => crate::theme::Mode::Dark,
                    crate::theme::Mode::Dark => crate::theme::Mode::Light,
                };
                set_theme(next, app);
            },
        ));
        let settings_selected = matches!(selected, NavTarget::Settings);
        container = container.child(activity_item(
            "settings",
            icons::settings(),
            settings_selected,
            accent,
            transparent,
            Some(SharedString::from("设置")),
            cx.listener(|this, _: &ClickEvent, _, cx| {
                this.navigate(NavTarget::Settings, cx);
            }),
        ));

        container
    }
}

/// 切浅 / 深主题 + 持久化
fn set_theme(mode: crate::theme::Mode, app: &mut gpui::App) {
    if crate::theme::current_mode(app) == mode {
        return;
    }
    crate::theme::apply_theme(mode, app);
    app.refresh_windows();
    persist_theme_pref(
        app,
        match mode {
            crate::theme::Mode::Dark => "dark",
            crate::theme::Mode::Light => "light",
        },
    );
}

/// 主题偏好落 redb（后台异步，失败仅告警不阻断 UI）
fn persist_theme_pref(app: &mut gpui::App, value: &'static str) {
    crate::preferences::persist_preference_latest("theme_mode", value.to_string(), app);
}

/// 选中时左侧 2px accent 竖条
fn activity_item(
    id: &str,
    icon: Icon,
    is_selected: bool,
    accent: gpui::Hsla,
    transparent: gpui::Hsla,
    tooltip: Option<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let mut button = crate::clickable_button(SharedString::from(id.to_string()))
        .ghost()
        .icon(icon);
    if let Some(tip) = tooltip {
        button = button.tooltip(tip);
    }
    h_flex()
        .w(px(BAR_WIDTH))
        .h(px(ITEM_HEIGHT))
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(2.0))
                .h(px(20.0))
                .bg(if is_selected { accent } else { transparent }),
        )
        .child(button.on_click(on_click))
}
