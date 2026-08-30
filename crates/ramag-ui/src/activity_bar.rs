use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Anchor, App, AppContext as _, BorrowAppContext as _, ClickEvent, Context, DragMoveEvent,
    EventEmitter, Global, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled,
    Subscription, Window, div, hsla, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, WindowExt as _,
    animation::{Transition, ease_in_out_cubic},
    badge::Badge,
    button::ButtonVariants as _,
    h_flex,
    notification::Notification,
    v_flex,
};
use ramag_app::{ToolRegistry, UpdateCheckResult};

use crate::PointerDropdownMenu as _;
use crate::icons;
use crate::tool_layout::{
    ACTIVITY_ITEM_GAP, DRAGGED_ITEM_OPACITY, ToolDrag, ToolDragGlobal, ToolDragPreview,
    ToolDragSurface, ToolDropSide, ToolLayoutGlobal, activity_drop_indicator,
    activity_drop_target_from_position, activity_reorder_animation_offset, begin_tool_drag,
    clear_tool_drag, dragged_item_background, notify_tool_layout_changed, persist_tool_order,
    tool_drag_display_slots, tool_drag_state, tool_drop_index, update_tool_drop_target,
};

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
    last_rendered_slots: Vec<Option<String>>,
    _update_indicator_subscription: Subscription,
    _tool_layout_subscription: Subscription,
    _tool_drag_subscription: Subscription,
}

/// 应用内更新角标状态；新版本可用时显示 1，否则隐藏。
#[derive(Clone, Copy, Default)]
pub(crate) struct UpdateIndicatorGlobal {
    pub(crate) available: bool,
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

type ActivityClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>;

struct ActivityItemConfig {
    id: SharedString,
    icon: Icon,
    is_selected: bool,
    accent: gpui::Hsla,
    decoration: ActivityItemDecoration,
    on_click: ActivityClickHandler,
    tool_drag: Option<ToolDrag>,
    source_index: Option<usize>,
    source_background: Option<gpui::Hsla>,
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
        cx.update_default_global::<ToolDragGlobal, _>(|_, _| {});
        let update_indicator_subscription =
            cx.observe_global::<UpdateIndicatorGlobal>(|_, cx| cx.notify());
        let tool_layout_subscription = cx.observe_global::<ToolLayoutGlobal>(|_, cx| cx.notify());
        let tool_drag_subscription = cx.observe_global::<ToolDragGlobal>(|_, cx| cx.notify());
        let last_rendered_slots = registry
            .list()
            .into_iter()
            .map(|tool| Some(tool.meta().id.clone()))
            .collect();
        Self {
            registry,
            selected: NavTarget::Home,
            last_rendered_slots,
            _update_indicator_subscription: update_indicator_subscription,
            _tool_layout_subscription: tool_layout_subscription,
            _tool_drag_subscription: tool_drag_subscription,
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

    /// 在释放时一次性提交最终槽位，保存偏好并通知首页与侧栏同步重绘。
    fn complete_drop(&mut self, dragged_id: &str, fallback_index: usize, cx: &mut Context<Self>) {
        let target_index = tool_drop_index(ToolDragSurface::ActivityBar, fallback_index, cx);
        if self.registry.reorder_to_index(dragged_id, target_index) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
        clear_tool_drag(cx);
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
    /// 将持久化工具顺序绘制为侧栏入口，并同步拖拽占位线与来源入口反馈。
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tools = self.registry.list();
        let selected = self.selected.clone();
        let tool_order = tools
            .iter()
            .map(|tool| tool.meta().id.clone())
            .collect::<Vec<_>>();
        let drag_state = tool_drag_state(cx);
        let display_slots =
            tool_drag_display_slots(&tool_order, ToolDragSurface::ActivityBar, &drag_state);
        let previous_slots =
            std::mem::replace(&mut self.last_rendered_slots, display_slots.clone());

        let accent = theme.accent;
        let target = drag_state
            .target
            .as_ref()
            .filter(|target| target.surface == ToolDragSurface::ActivityBar)
            .map(|target| (target.index, target.side));
        let item_count = tool_order.len();
        let update_available =
            cx.read_global::<UpdateIndicatorGlobal, _>(|state, _| state.available);
        let sidebar_bg = theme.sidebar;
        let border = theme.border;

        let mut tool_items = Vec::with_capacity(display_slots.len());
        for slot in &display_slots {
            let Some(id) = slot else {
                continue;
            };

            let Some(tool) = tools.iter().find(|tool| tool.meta().id == *id) else {
                continue;
            };
            let Some(source_index) = tool_order.iter().position(|item| item == id) else {
                continue;
            };
            let id_for_click = id.clone();
            let is_selected = matches!(&selected, NavTarget::Tool(s) if s == id);
            let is_dragged = drag_state.dragged_id.as_deref() == Some(id.as_str());
            let mut item = activity_item(
                ActivityItemConfig {
                    id: format!("tool-{id}").into(),
                    icon: Self::icon_for_tool(id),
                    is_selected,
                    accent,
                    decoration: ActivityItemDecoration::new(
                        SharedString::from(tool.meta().name.clone()),
                        false,
                    ),
                    on_click: Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.navigate(NavTarget::Tool(id_for_click.clone()), cx);
                    })),
                    tool_drag: Some(ToolDrag { id: id.clone() }),
                    source_index: Some(source_index),
                    source_background: is_dragged.then(|| dragged_item_background(sidebar_bg)),
                },
                cx,
            );
            if is_dragged {
                item = item.opacity(DRAGGED_ITEM_OPACITY);
            }
            let offset = activity_reorder_animation_offset(&previous_slots, &display_slots, id);
            let item = if offset == px(0.0) {
                item.into_any_element()
            } else {
                Transition::new(Duration::from_millis(360))
                    .ease(ease_in_out_cubic)
                    .slide_y(offset, px(0.0))
                    .apply(
                        item,
                        format!("activity-tool-reorder-{id}-{}", drag_state.revision),
                    )
                    .into_any_element()
            };
            tool_items.push(item);
        }

        let drop_end_index = item_count;
        let drop_end_target = drop_end_index.saturating_sub(1);
        let mut tool_list = v_flex()
            .id("activity-tool-list")
            .w(px(BAR_WIDTH))
            .flex_none()
            .gap(px(ACTIVITY_ITEM_GAP))
            .relative()
            .on_drag_move(
                cx.listener(move |_, event: &DragMoveEvent<ToolDrag>, _, cx| {
                    let local_y = f32::from(event.event.position.y - event.bounds.top());
                    if let Some((target_index, side)) =
                        activity_drop_target_from_position(local_y, item_count)
                    {
                        update_tool_drop_target(
                            ToolDragSurface::ActivityBar,
                            target_index,
                            side,
                            cx,
                        );
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                this.complete_drop(&drag.id, item_count, cx);
            }))
            .children(tool_items)
            .child(
                div()
                    .id("activity-tool-drop-end")
                    .w(px(BAR_WIDTH))
                    .h(px(8.0))
                    .flex_none()
                    .on_mouse_move(cx.listener(move |_, _, _, cx| {
                        update_tool_drop_target(
                            ToolDragSurface::ActivityBar,
                            drop_end_target,
                            ToolDropSide::Bottom,
                            cx,
                        );
                    }))
                    .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                        this.complete_drop(&drag.id, drop_end_index, cx);
                    })),
            );
        if let Some((target_index, target_side)) = target
            && let Some(indicator) = activity_drop_indicator(
                accent,
                drag_state.source_index,
                target_index,
                target_side,
                item_count,
            )
        {
            tool_list = tool_list.child(
                indicator
                    .id("activity-tool-drop-indicator")
                    .on_mouse_move(cx.listener(move |_, _, _, cx| {
                        update_tool_drop_target(
                            ToolDragSurface::ActivityBar,
                            target_index,
                            target_side,
                            cx,
                        );
                    }))
                    .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                        this.complete_drop(&drag.id, target_index, cx);
                    })),
            );
        }

        let mut container = v_flex()
            .w(px(BAR_WIDTH))
            .h_full()
            .flex_none()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .py_2()
            .gap_1()
            .items_center()
            .on_mouse_up(MouseButton::Left, |_, _, cx| clear_tool_drag(cx));

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
                source_index: None,
                source_background: None,
            },
            cx,
        ));

        container = container.child(div().w(px(20.0)).h(px(1.0)).bg(border).my_1());
        if !tools.is_empty() {
            container = container.child(tool_list);
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
                source_index: None,
                source_background: None,
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
                source_index: None,
                source_background: None,
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

fn activity_item(
    config: ActivityItemConfig,
    cx: &mut Context<ActivityBar>,
) -> gpui::Stateful<gpui::Div> {
    let ActivityItemConfig {
        id,
        icon,
        is_selected,
        accent,
        decoration,
        on_click,
        tool_drag,
        source_index,
        source_background,
    } = config;
    let ActivityItemDecoration {
        tooltip,
        show_badge,
    } = decoration;
    let transparent = hsla(0.0, 0.0, 0.0, 0.0);
    let preview_icon = icon.clone();
    let preview_name = tooltip.clone();
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
        .bg(source_background.unwrap_or(transparent))
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
        let Some(source_index) = source_index else {
            return item;
        };
        item = item
            .cursor_move()
            .on_drag(drag, move |drag, position, _, cx| {
                begin_tool_drag(&drag.id, ToolDragSurface::ActivityBar, source_index, cx);
                cx.new(|_| {
                    ToolDragPreview::new(
                        ToolDragSurface::ActivityBar,
                        preview_icon.clone(),
                        preview_name.to_string(),
                        String::new(),
                    )
                    .position(position)
                })
            })
            .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                this.complete_drop(&drag.id, source_index, cx);
            }));
    }
    item
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
