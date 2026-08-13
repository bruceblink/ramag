use std::sync::Arc;

use gpui::{
    App, AppContext as _, BorrowAppContext as _, ClickEvent, Context, EventEmitter, Global,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, hsla, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, badge::Badge, button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_app::{ToolRegistry, UpdateCheckResult};

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
    _update_indicator_subscription: Subscription,
}

/// 应用内更新角标状态；新版本可用时显示 1，否则隐藏。
#[derive(Clone, Copy, Default)]
struct UpdateIndicatorGlobal {
    available: bool,
}

impl Global for UpdateIndicatorGlobal {}

struct ActivityItemDecoration {
    tooltip: SharedString,
    show_badge: bool,
}

impl ActivityItemDecoration {
    fn new(tooltip: impl Into<SharedString>, show_badge: bool) -> Self {
        Self {
            tooltip: tooltip.into(),
            show_badge,
        }
    }
}

/// 将更新检查结果同步到设置入口角标。
pub fn sync_update_indicator(result: &UpdateCheckResult, cx: &mut App) {
    let available = indicator_value(result);
    let current = cx
        .try_global::<UpdateIndicatorGlobal>()
        .is_some_and(|state| state.available);
    if current != available {
        cx.set_global(UpdateIndicatorGlobal { available });
    }
}

fn indicator_value(result: &UpdateCheckResult) -> bool {
    match result {
        UpdateCheckResult::UpToDate { .. } => false,
        UpdateCheckResult::Available(_) | UpdateCheckResult::UnsupportedPlatform(_) => true,
    }
}

impl EventEmitter<NavEvent> for ActivityBar {}

impl ActivityBar {
    pub fn new(registry: Arc<ToolRegistry>, cx: &mut Context<Self>) -> Self {
        cx.update_default_global::<UpdateIndicatorGlobal, _>(|_, _| {});
        let update_indicator_subscription =
            cx.observe_global::<UpdateIndicatorGlobal>(|_, cx| cx.notify());
        Self {
            registry,
            selected: NavTarget::Home,
            _update_indicator_subscription: update_indicator_subscription,
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

    /// 首页复用此映射，保证入口图标一致。
    pub(crate) fn icon_for_tool(tool_id: &str) -> Icon {
        match tool_id {
            "dbclient" => icons::database(),
            "vcs" => icons::git_branch(),
            "clipboard" => icons::clipboard(),
            "ssh" => Icon::new(IconName::SquareTerminal),
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
        let update_available =
            cx.read_global::<UpdateIndicatorGlobal, _>(|state, _| state.available);
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
            ActivityItemDecoration::new("首页", false),
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
                ActivityItemDecoration::new(tip, false),
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.navigate(NavTarget::Tool(id_for_click.clone()), cx);
                }),
            ));
        }

        container = container.child(div().flex_1());
        container = container.child(activity_item(
            "shortcuts",
            crate::shortcuts_dialog::shortcut_icon(),
            false,
            accent,
            transparent,
            ActivityItemDecoration::new("快捷键", false),
            |_: &ClickEvent, window, app| crate::shortcuts_dialog::open_shortcuts(window, app),
        ));
        let (theme_icon, theme_tip) =
            if matches!(crate::theme::current_mode(cx), crate::theme::Mode::Light) {
                (IconName::Sun, "切换外观")
            } else {
                (IconName::Moon, "切换外观")
            };
        container = container.child(activity_item(
            "theme-toggle",
            Icon::new(theme_icon),
            false,
            accent,
            transparent,
            ActivityItemDecoration::new(theme_tip, false),
            |_: &ClickEvent, _, app| {
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
            ActivityItemDecoration::new("设置", update_available),
            cx.listener(|this, _: &ClickEvent, _, cx| {
                this.navigate(NavTarget::Settings, cx);
            }),
        ));

        container
    }
}

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

fn activity_item(
    id: &str,
    icon: Icon,
    is_selected: bool,
    accent: gpui::Hsla,
    transparent: gpui::Hsla,
    decoration: ActivityItemDecoration,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let ActivityItemDecoration {
        tooltip,
        show_badge,
    } = decoration;
    let mut button = crate::clickable_button(SharedString::from(id.to_string())).ghost();
    button = if !show_badge {
        button.icon(icon)
    } else {
        button
            .size(px(32.0))
            .p_0()
            .child(Badge::new().dot().color(accent).child(icon))
    };
    button = button.tooltip(tooltip);
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

#[cfg(test)]
mod tests {
    use ramag_app::AvailableUpdate;
    use ramag_domain::entities::ReleaseInfo;

    use super::{UpdateCheckResult, indicator_value};

    fn available_result() -> UpdateCheckResult {
        UpdateCheckResult::Available(AvailableUpdate {
            release: ReleaseInfo {
                version: "0.0.3".into(),
                tag_name: "v0.0.3".into(),
                release_url: "https://github.com/tools-rs/ramag/releases/tag/v0.0.3".into(),
                notes: String::new(),
                published_at: None,
                assets: Vec::new(),
            },
            asset: None,
        })
    }

    #[test]
    fn update_indicator_tracks_only_real_update_results() {
        assert!(!indicator_value(&UpdateCheckResult::UpToDate {
            current_version: "0.0.2".into(),
            latest_version: "0.0.2".into(),
        }));
        assert!(indicator_value(&available_result()));
    }
}
