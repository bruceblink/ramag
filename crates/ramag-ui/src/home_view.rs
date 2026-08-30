use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, BorrowAppContext as _, ClickEvent, Context, DragMoveEvent, EventEmitter,
    IntoElement, MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, hsla, prelude::*, px,
};
use gpui_component::{
    ActiveTheme,
    animation::{Transition, ease_in_out_cubic},
    h_flex, v_flex,
};

use ramag_app::ToolRegistry;

use crate::activity_bar::ActivityBar;
use crate::tool_layout::{
    DRAGGED_ITEM_OPACITY, HomeDropLayout, ToolDrag, ToolDragGlobal, ToolDragPreview,
    ToolDragSurface, ToolDropSide, ToolLayoutGlobal, begin_tool_drag, clear_tool_drag,
    home_drop_indicator, home_drop_target_from_position, notify_tool_layout_changed,
    persist_tool_order, tool_drag_display_slots, tool_drag_handle, tool_drag_state,
    tool_drop_index, update_tool_drop_target,
};

#[derive(Debug, Clone)]
pub enum HomeEvent {
    OpenTool(String),
}

const RAMAG_LOGO: &[&str] = &[
    "██████╗  █████╗ ███╗   ███╗ █████╗  ██████╗ ",
    "██╔══██╗██╔══██╗████╗ ████║██╔══██╗██╔════╝ ",
    "██████╔╝███████║██╔████╔██║███████║██║  ███╗",
    "██╔══██╗██╔══██║██║╚██╔╝██║██╔══██║██║   ██║",
    "██║  ██║██║  ██║██║ ╚═╝ ██║██║  ██║╚██████╔╝",
    "╚═╝  ╚═╝╚═╝  ╚═╝╚═╝     ╚═╝╚═╝  ╚═╝ ╚═════╝ ",
];
const TOOL_CARD_WIDTH: f32 = 280.0;
const TOOL_CARD_HEIGHT: f32 = 112.0;
const TOOL_CARD_GAP: f32 = 16.0;
const TOOL_GRID_WIDTH: f32 = TOOL_CARD_WIDTH * 3.0 + TOOL_CARD_GAP * 2.0;

pub struct HomeView {
    registry: Arc<ToolRegistry>,
    last_rendered_slots: Vec<Option<String>>,
    _tool_layout_subscription: Subscription,
    _tool_drag_subscription: Subscription,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(registry: Arc<ToolRegistry>, cx: &mut Context<Self>) -> Self {
        cx.update_default_global::<ToolLayoutGlobal, _>(|_, _| {});
        cx.update_default_global::<ToolDragGlobal, _>(|_, _| {});
        let tool_layout_subscription = cx.observe_global::<ToolLayoutGlobal>(|_, cx| cx.notify());
        let tool_drag_subscription = cx.observe_global::<ToolDragGlobal>(|_, cx| cx.notify());
        let last_rendered_slots = registry
            .list()
            .into_iter()
            .map(|tool| Some(tool.meta().id.clone()))
            .collect();
        Self {
            registry,
            last_rendered_slots,
            _tool_layout_subscription: tool_layout_subscription,
            _tool_drag_subscription: tool_drag_subscription,
        }
    }

    /// 在释放时一次性提交最终槽位，保存偏好并通知首页与侧栏同步重绘。
    fn complete_drop(&mut self, dragged_id: &str, fallback_index: usize, cx: &mut Context<Self>) {
        let target_index = tool_drop_index(ToolDragSurface::Home, fallback_index, cx);
        if self.registry.reorder_to_index(dragged_id, target_index) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
        clear_tool_drag(cx);
    }
}

impl Render for HomeView {
    /// 将持久化工具顺序绘制为首页网格，并同步拖拽占位线与来源卡片反馈。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;
        let mono = theme.mono_font_family.clone();
        let bg = theme.background;
        let border = theme.border;
        let fg = theme.foreground;
        let card_bg = theme.secondary;

        let mut accent_border = accent;
        accent_border.a = 0.55;

        let tools = self.registry.list();
        let has_tools = !tools.is_empty();
        let current_order = tools
            .iter()
            .map(|tool| tool.meta().id.clone())
            .collect::<Vec<_>>();
        let drag_state = tool_drag_state(cx);
        let display_slots =
            tool_drag_display_slots(&current_order, ToolDragSurface::Home, &drag_state);
        let previous_slots =
            std::mem::replace(&mut self.last_rendered_slots, display_slots.clone());
        let layout_revision = cx.read_global::<ToolLayoutGlobal, _>(|state, _| state.revision);
        let columns = grid_columns(f32::from(window.bounds().size.width));
        let item_count = current_order.len();
        let home_layout = HomeDropLayout {
            width: TOOL_CARD_WIDTH,
            height: TOOL_CARD_HEIGHT,
            item_count,
            columns,
            gap: TOOL_CARD_GAP,
        };
        let target = drag_state
            .target
            .as_ref()
            .filter(|target| target.surface == ToolDragSurface::Home)
            .map(|target| (target.index, target.side));
        let mut cards = Vec::with_capacity(display_slots.len());
        for slot in &display_slots {
            let Some(id) = slot else {
                continue;
            };

            let Some(tool) = tools.iter().find(|tool| tool.meta().id == *id) else {
                continue;
            };
            let Some(source_index) = current_order.iter().position(|item| item == id) else {
                continue;
            };
            let card_id = SharedString::from(format!("home-tool-{id}"));
            let id_for_click = id.clone();
            let name = tool.meta().name.clone();
            let description = tool.meta().description.clone();
            let icon = ActivityBar::icon_for_tool(id);
            let preview_icon = icon.clone();
            let preview_name = name.clone();
            let preview_description = description.clone();
            let drag = ToolDrag { id: id.clone() };
            let is_dragged = drag_state.dragged_id.as_deref() == Some(id.as_str());

            let mut card = v_flex()
                .id(card_id)
                .w(px(TOOL_CARD_WIDTH))
                .h(px(TOOL_CARD_HEIGHT))
                .p(px(20.0))
                .gap(px(10.0))
                .bg(card_bg)
                .border_1()
                .border_color(border)
                .rounded(px(10.0))
                .relative()
                .cursor_move()
                .hover(move |this| this.border_color(accent_border))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    cx.emit(HomeEvent::OpenTool(id_for_click.clone()));
                }))
                .on_drag(drag, move |drag, position, _, cx| {
                    begin_tool_drag(&drag.id, ToolDragSurface::Home, source_index, cx);
                    cx.new(|_| {
                        ToolDragPreview::new(
                            ToolDragSurface::Home,
                            preview_icon.clone(),
                            preview_name.clone(),
                            preview_description.clone(),
                        )
                        .position(position)
                    })
                })
                .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                    this.complete_drop(&drag.id, source_index, cx);
                }))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().text_color(accent).child(icon))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(fg)
                                .child(name),
                        ),
                )
                .child(div().text_xs().text_color(muted_fg).child(description))
                .child(
                    div()
                        .absolute()
                        .top(px(10.0))
                        .right(px(10.0))
                        .child(tool_drag_handle(accent.opacity(0.65))),
                );
            if is_dragged {
                card = card.opacity(DRAGGED_ITEM_OPACITY);
            }
            let (from_x, from_y) =
                reorder_animation_offset(&previous_slots, &display_slots, id, columns);
            let card = if from_x == px(0.0) && from_y == px(0.0) {
                card.into_any_element()
            } else {
                Transition::new(Duration::from_millis(360))
                    .ease(ease_in_out_cubic)
                    .slide_x(from_x, px(0.0))
                    .slide_y(from_y, px(0.0))
                    .apply(
                        card,
                        format!(
                            "home-tool-reorder-{id}-{}-{layout_revision}",
                            drag_state.revision
                        ),
                    )
                    .into_any_element()
            };
            cards.push(card);
        }

        let mut tool_grid = div()
            .id("home-tool-grid")
            .w_full()
            .max_w(px(TOOL_GRID_WIDTH))
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_start()
            .gap(px(TOOL_CARD_GAP))
            .relative()
            .on_drag_move(
                cx.listener(move |_, event: &DragMoveEvent<ToolDrag>, _, cx| {
                    let local_x = f32::from(event.event.position.x - event.bounds.left());
                    let local_y = f32::from(event.event.position.y - event.bounds.top());
                    if let Some((target_index, side)) = home_drop_target_from_position(
                        local_x,
                        local_y,
                        drag_state.source_index,
                        home_layout,
                    ) {
                        update_tool_drop_target(ToolDragSurface::Home, target_index, side, cx);
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                this.complete_drop(&drag.id, item_count, cx);
            }))
            .children(cards);
        if let Some((target_index, target_side)) = target
            && let Some(indicator) = home_drop_indicator(
                accent,
                drag_state.source_index,
                target_index,
                target_side,
                home_layout,
            )
        {
            tool_grid = tool_grid.child(
                indicator
                    .id("home-tool-drop-indicator")
                    .on_mouse_move(cx.listener(move |_, _, _, cx| {
                        update_tool_drop_target(
                            ToolDragSurface::Home,
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
        if has_tools {
            let drop_end_index = item_count;
            let drop_end_target = drop_end_index - 1;
            tool_grid = tool_grid.child(
                div()
                    .id("home-tool-drop-end")
                    .w_full()
                    .h(px(10.0))
                    .flex_none()
                    .on_mouse_move(cx.listener(move |_, _, _, cx| {
                        update_tool_drop_target(
                            ToolDragSurface::Home,
                            drop_end_target,
                            ToolDropSide::Bottom,
                            cx,
                        );
                    }))
                    .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                        this.complete_drop(&drag.id, drop_end_index, cx);
                    })),
            );
        }

        v_flex()
            .size_full()
            .bg(bg)
            .items_center()
            .justify_center()
            .on_mouse_up(MouseButton::Left, |_, _, cx| clear_tool_drag(cx))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(960.0))
                    .p(px(32.0))
                    .gap(px(36.0))
                    .items_center()
                    .child(render_logo(mono, accent))
                    .child(tool_grid),
            )
    }
}

/// 根据窗口宽度计算首页网格列数，动画偏移必须与实际换行规则一致。
fn grid_columns(window_width: f32) -> usize {
    let content_width = (window_width - 48.0 - 64.0).min(960.0 - 64.0);
    ((content_width.max(TOOL_CARD_WIDTH) + TOOL_CARD_GAP) / (TOOL_CARD_WIDTH + TOOL_CARD_GAP))
        .floor()
        .clamp(1.0, 3.0) as usize
}

/// 计算卡片从旧网格槽位移动到新槽位时的相对起点。
fn reorder_animation_offset(
    previous_slots: &[Option<String>],
    current_slots: &[Option<String>],
    id: &str,
    columns: usize,
) -> (gpui::Pixels, gpui::Pixels) {
    let Some(previous_index) = previous_slots
        .iter()
        .position(|slot| slot.as_deref() == Some(id))
    else {
        return (px(0.0), px(0.0));
    };
    let Some(current_index) = current_slots
        .iter()
        .position(|slot| slot.as_deref() == Some(id))
    else {
        return (px(0.0), px(0.0));
    };
    if previous_index == current_index {
        return (px(0.0), px(0.0));
    }

    let previous_column = previous_index % columns;
    let previous_row = previous_index / columns;
    let current_column = current_index % columns;
    let current_row = current_index / columns;
    (
        px((previous_column as f32 - current_column as f32) * (TOOL_CARD_WIDTH + TOOL_CARD_GAP)),
        px((previous_row as f32 - current_row as f32) * (TOOL_CARD_HEIGHT + TOOL_CARD_GAP)),
    )
}

fn render_logo(mono: SharedString, accent: gpui::Hsla) -> impl IntoElement {
    let mut lines = Vec::with_capacity(RAMAG_LOGO.len());
    for (i, line) in RAMAG_LOGO.iter().enumerate() {
        let alpha = 1.0 - (i as f32) * 0.06;
        let color = hsla(accent.h, accent.s, accent.l, alpha);
        lines.push(
            div()
                .text_color(color)
                .line_height(px(13.0))
                .child(SharedString::from(line.to_string())),
        );
    }

    v_flex()
        .items_center()
        .font_family(mono)
        .text_size(px(14.0))
        .font_weight(gpui::FontWeight::BOLD)
        .children(lines)
}

#[cfg(test)]
mod tests {
    use super::{grid_columns, reorder_animation_offset};

    #[test]
    fn grid_columns_follow_the_fixed_card_width() {
        assert_eq!(grid_columns(1689.0), 3);
        assert_eq!(grid_columns(900.0), 2);
        assert_eq!(grid_columns(600.0), 1);
    }

    #[test]
    fn reorder_animation_starts_at_the_previous_grid_slot() {
        let previous = vec![
            Some("left".to_owned()),
            Some("right".to_owned()),
            Some("bottom".to_owned()),
        ];
        let current = vec![
            Some("right".to_owned()),
            Some("left".to_owned()),
            Some("bottom".to_owned()),
        ];

        let right_offset = reorder_animation_offset(&previous, &current, "right", 2);
        assert_eq!(f32::from(right_offset.0), 296.0);
        assert_eq!(f32::from(right_offset.1), 0.0);

        let left_offset = reorder_animation_offset(&previous, &current, "left", 2);
        assert_eq!(f32::from(left_offset.0), -296.0);
        assert_eq!(f32::from(left_offset.1), 0.0);
    }
}
