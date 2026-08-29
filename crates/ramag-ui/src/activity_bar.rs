use std::sync::Arc;

use gpui::{
    Anchor, App, AppContext as _, BorrowAppContext as _, ClickEvent, Context, EventEmitter, Global,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, hsla,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, WindowExt as _, badge::Badge, button::ButtonVariants as _, h_flex,
    notification::Notification, v_flex,
};
use ramag_app::{ToolRegistry, UpdateCheckResult};

use crate::PointerDropdownMenu as _;
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

/// 拖拽工具入口时在首页和侧栏之间传递的最小数据。
#[derive(Debug, Clone)]
pub(crate) struct ToolDrag {
    pub(crate) id: String,
    pub(crate) name: SharedString,
    pub(crate) description: SharedString,
}

/// 工具顺序变化通知；注册表保存顺序，Global 只负责让各个视图重绘。
#[derive(Clone, Copy, Default)]
pub(crate) struct ToolLayoutGlobal {
    pub(crate) revision: u64,
}

impl Global for ToolLayoutGlobal {}

pub struct ActivityBar {
    registry: Arc<ToolRegistry>,
    selected: NavTarget,
    _update_indicator_subscription: Subscription,
    _tool_layout_subscription: Subscription,
}

/// 应用内更新角标状态；新版本可用时显示 1，否则隐藏。
#[derive(Clone, Copy, Default)]
pub(crate) struct UpdateIndicatorGlobal {
    pub(crate) available: bool,
}

impl Global for UpdateIndicatorGlobal {}

/// 通知所有观察工具布局的视图重新读取注册表顺序。
pub(crate) fn notify_tool_layout_changed(cx: &mut App) {
    let revision = cx
        .try_global::<ToolLayoutGlobal>()
        .map_or(1, |state| state.revision.wrapping_add(1));
    cx.set_global(ToolLayoutGlobal { revision });
}

/// 将完整工具 ID 顺序异步写入偏好，隐藏工具也会被保留以兼容不同平台。
pub(crate) fn persist_tool_order(registry: &ToolRegistry, cx: &mut App) {
    let order = registry.order();
    match serde_json::to_string(&order) {
        Ok(value) => {
            crate::preferences::persist_preference_latest(ramag_app::TOOL_ORDER_PREF_KEY, value, cx)
        }
        Err(error) => tracing::warn!(
            operation = "tool_order_save",
            error = %error,
            "serialize tool layout failed"
        ),
    }
}

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

type ActivityClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>;

struct ActivityItemConfig {
    id: SharedString,
    icon: Icon,
    is_selected: bool,
    accent: gpui::Hsla,
    decoration: ActivityItemDecoration,
    on_click: ActivityClickHandler,
    tool_drag: Option<ToolDrag>,
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
        UpdateCheckResult::Available(_) => true,
        UpdateCheckResult::UnsupportedPlatform(_) => false,
    }
}

impl EventEmitter<NavEvent> for ActivityBar {}

impl ActivityBar {
    pub fn new(registry: Arc<ToolRegistry>, cx: &mut Context<Self>) -> Self {
        cx.update_default_global::<UpdateIndicatorGlobal, _>(|_, _| {});
        cx.update_default_global::<ToolLayoutGlobal, _>(|_, _| {});
        let update_indicator_subscription =
            cx.observe_global::<UpdateIndicatorGlobal>(|_, cx| cx.notify());
        let tool_layout_subscription = cx.observe_global::<ToolLayoutGlobal>(|_, cx| cx.notify());
        Self {
            registry,
            selected: NavTarget::Home,
            _update_indicator_subscription: update_indicator_subscription,
            _tool_layout_subscription: tool_layout_subscription,
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

    /// 将整项移动到目标项的位置，保存偏好并通知首页与侧栏同步重绘。
    fn complete_drop(&mut self, dragged_id: &str, target_id: &str, cx: &mut Context<Self>) {
        if self.registry.reorder_to_target(dragged_id, target_id) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
    }

    /// 将整项移动到工具列表末尾，支持拖到最后一个项目之后。
    fn complete_drop_to_end(&mut self, dragged_id: &str, cx: &mut Context<Self>) {
        if self.registry.move_to_end(dragged_id) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
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
            ActivityItemConfig {
                id: "home".into(),
                icon: icons::home(),
                is_selected: is_home_selected,
                accent,
                decoration: ActivityItemDecoration::new("首页", false),
                on_click: Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.navigate(NavTarget::Home, cx);
                })),
                tool_drag: None,
            },
            cx,
        ));

        container = container.child(div().w(px(20.0)).h(px(1.0)).bg(border).my_1());

        for tool in tools.iter() {
            let id = tool.meta().id.clone();
            let id_for_click = id.clone();
            let is_selected = matches!(&selected, NavTarget::Tool(s) if s == &id);
            let icon = Self::icon_for_tool(&id);
            let name = SharedString::from(tool.meta().name.clone());
            let description = SharedString::from(tool.meta().description.clone());

            container = container.child(activity_item(
                ActivityItemConfig {
                    id: format!("tool-{id}").into(),
                    icon,
                    is_selected,
                    accent,
                    decoration: ActivityItemDecoration::new(name.clone(), false),
                    on_click: Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.navigate(NavTarget::Tool(id_for_click.clone()), cx);
                    })),
                    tool_drag: Some(ToolDrag {
                        id,
                        name,
                        description,
                    }),
                },
                cx,
            ));
        }

        if !tools.is_empty() {
            container = container.child(
                div()
                    .id("activity-tool-drop-end")
                    .w(px(BAR_WIDTH))
                    .h(px(8.0))
                    .flex_none()
                    .rounded(px(4.0))
                    .drag_over::<ToolDrag>(move |style, _, _, _| style.bg(accent.opacity(0.16)))
                    .on_drop(cx.listener(|this, drag: &ToolDrag, _, cx| {
                        this.complete_drop_to_end(&drag.id, cx);
                    })),
            );
        }

        container = container.child(div().flex_1());
        container = container.child(
            crate::clickable_button("add-menu")
                .ghost()
                .icon(Icon::new(IconName::Plus))
                .tooltip("添加")
                .pointer_dropdown_menu_with_anchor(Anchor::TopLeft, |mut menu, _, _| {
                    menu = menu.item(crate::menu_item("添加工具").on_click(|_, window, cx| {
                        show_add_placeholder("添加工具", window, cx);
                    }));
                    menu.item(crate::menu_item("添加 MCP").on_click(|_, window, cx| {
                        show_add_placeholder("添加 MCP", window, cx);
                    }))
                }),
        );
        container = container.child(activity_item(
            ActivityItemConfig {
                id: "shortcuts".into(),
                icon: crate::shortcuts_dialog::shortcut_icon(),
                is_selected: false,
                accent,
                decoration: ActivityItemDecoration::new("快捷键", false),
                on_click: Box::new(|_: &ClickEvent, window, app| {
                    crate::shortcuts_dialog::open_shortcuts(window, app)
                }),
                tool_drag: None,
            },
            cx,
        ));
        let settings_selected = matches!(selected, NavTarget::Settings);
        container = container.child(activity_item(
            ActivityItemConfig {
                id: "settings".into(),
                icon: icons::settings(),
                is_selected: settings_selected,
                accent,
                decoration: ActivityItemDecoration::new("设置", update_available),
                on_click: Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.navigate(NavTarget::Settings, cx);
                })),
                tool_drag: None,
            },
            cx,
        ));

        container
    }
}

/// 添加入口暂时没有对应的工具创建流程，先用统一通知明确反馈点击结果。
fn show_add_placeholder(kind: &str, window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::info(format!("{kind}功能即将支持")).autohide(true),
        cx,
    );
}

fn activity_item(config: ActivityItemConfig, cx: &mut Context<ActivityBar>) -> impl IntoElement {
    let ActivityItemConfig {
        id,
        icon,
        is_selected,
        accent,
        decoration,
        on_click,
        tool_drag,
    } = config;
    let ActivityItemDecoration {
        tooltip,
        show_badge,
    } = decoration;
    let transparent = hsla(0.0, 0.0, 0.0, 0.0);
    let mut button = crate::clickable_button(id.clone()).ghost();
    button = if !show_badge {
        button.icon(icon)
    } else {
        button
            .size(px(32.0))
            .p_0()
            .child(Badge::new().dot().color(accent).child(icon))
    };
    button = button.tooltip(tooltip);
    let mut item = h_flex()
        .id(SharedString::from(format!("activity-{id}")))
        .w(px(BAR_WIDTH))
        .h(px(ITEM_HEIGHT))
        .relative()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(2.0))
                .h(px(20.0))
                .bg(if is_selected { accent } else { transparent }),
        )
        .child(button.on_click(on_click));
    if let Some(drag) = tool_drag {
        let target_id = drag.id.clone();
        let target_id_for_drop = target_id.clone();
        item = item
            .cursor_move()
            .on_drag(drag, |_, _, _, cx| cx.new(|_| ToolDragPreviewPlaceholder))
            .drag_over::<ToolDrag>(move |style, drag, _, _| {
                if drag.id == target_id {
                    style
                } else {
                    style.bg(accent.opacity(0.16))
                }
            })
            .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                this.complete_drop(&drag.id, &target_id_for_drop, cx);
            }));
    }
    item
}

/// 侧栏拖拽仅传递排序数据，不绘制浮动预览；按钮自身仍保留原有 tooltip。
pub(crate) struct ToolDragPreviewPlaceholder;

impl Render for ToolDragPreviewPlaceholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.0))
    }
}

/// 用 2x3 点阵标识可排序卡片，拖拽命中区域仍是整个卡片。
pub(crate) fn tool_drag_handle(color: gpui::Hsla) -> impl IntoElement {
    v_flex().gap(px(2.0)).children((0..3).map(|_| {
        h_flex()
            .gap(px(2.0))
            .children((0..2).map(|_| div().size(px(3.0)).rounded(px(1.5)).bg(color)))
    }))
}

/// 拖拽预览复用完整卡片外观，不参与注册表状态更新。
pub(crate) struct ToolDragPreview {
    pub(crate) id: String,
    pub(crate) name: SharedString,
    pub(crate) description: SharedString,
}

impl Render for ToolDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w(px(280.0))
            .h(px(112.0))
            .p(px(20.0))
            .gap(px(10.0))
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.border)
            .rounded(px(10.0))
            .relative()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_color(theme.accent)
                            .child(ActivityBar::icon_for_tool(&self.id)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(self.name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.description.clone()),
            )
            .child(
                div()
                    .absolute()
                    .top(px(10.0))
                    .right(px(10.0))
                    .child(tool_drag_handle(theme.accent.opacity(0.65))),
            )
    }
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
        let available = available_result();
        assert!(indicator_value(&available));
        let UpdateCheckResult::Available(update) = available else {
            unreachable!();
        };
        assert!(!indicator_value(&UpdateCheckResult::UnsupportedPlatform(
            update
        )));
    }
}
