use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, BorrowAppContext as _, ClickEvent, Context, DragMoveEvent, EventEmitter,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, hsla,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme,
    animation::{Transition, ease_in_out_cubic},
    h_flex, v_flex,
};

use ramag_app::ToolRegistry;

use crate::activity_bar::{
    ToolDrag, ToolDragPreview, ToolLayoutGlobal, notify_tool_layout_changed, persist_tool_order,
    tool_drag_handle,
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
    last_rendered_order: Vec<String>,
    _tool_layout_subscription: Subscription,
}

impl EventEmitter<HomeEvent> for HomeView {}

impl HomeView {
    pub fn new(registry: Arc<ToolRegistry>, cx: &mut Context<Self>) -> Self {
        cx.update_default_global::<ToolLayoutGlobal, _>(|_, _| {});
        let tool_layout_subscription = cx.observe_global::<ToolLayoutGlobal>(|_, cx| cx.notify());
        let last_rendered_order = registry
            .list()
            .into_iter()
            .map(|tool| tool.meta().id.clone())
            .collect();
        Self {
            registry,
            last_rendered_order,
            _tool_layout_subscription: tool_layout_subscription,
        }
    }

    /// 将整张卡片移动到目标卡片的位置，并持久化新的工具顺序。
    fn complete_drop(&mut self, dragged_id: &str, target_id: &str, cx: &mut Context<Self>) {
        if self.registry.reorder_to_target(dragged_id, target_id) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
    }

    /// 将整张卡片移动到卡片网格末尾，并持久化新的工具顺序。
    fn complete_drop_to_end(&mut self, dragged_id: &str, cx: &mut Context<Self>) {
        if self.registry.move_to_end(dragged_id) {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
    }

    /// 根据拖拽指针所在的固定网格槽位即时调整顺序，让目标卡片同步让位。
    fn handle_drag_move(&mut self, event: &DragMoveEvent<ToolDrag>, cx: &mut Context<Self>) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }

        let dragged_id = event.drag(cx).id.clone();
        let tools = self.registry.list();
        if tools.len() < 2 {
            return;
        }

        let local_x = f32::from(event.event.position.x - event.bounds.left());
        let local_y = f32::from(event.event.position.y - event.bounds.top());
        if local_x < 0.0 || local_y < 0.0 {
            return;
        }

        let cell_width = TOOL_CARD_WIDTH + TOOL_CARD_GAP;
        let cell_height = TOOL_CARD_HEIGHT + TOOL_CARD_GAP;
        let columns = ((f32::from(event.bounds.size.width) + TOOL_CARD_GAP) / cell_width)
            .floor()
            .max(1.0) as usize;
        let column = (local_x / cell_width).floor() as usize;
        if column >= columns {
            return;
        }
        let row = (local_y / cell_height).floor() as usize;
        let slot = row.saturating_mul(columns).saturating_add(column);

        let changed = if slot >= tools.len() {
            self.registry.move_to_end(&dragged_id)
        } else {
            let target_id = tools[slot].meta().id.as_str();
            self.registry.reorder_to_target(&dragged_id, target_id)
        };
        if changed {
            persist_tool_order(&self.registry, cx);
            notify_tool_layout_changed(cx);
        }
    }
}

impl Render for HomeView {
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
        let previous_order =
            std::mem::replace(&mut self.last_rendered_order, current_order.clone());
        let layout_revision = cx.read_global::<ToolLayoutGlobal, _>(|state, _| state.revision);
        let columns = grid_columns(f32::from(window.bounds().size.width));
        let cards = tools.into_iter().map(|tool| {
            let id = tool.meta().id.clone();
            let card_id = SharedString::from(format!("home-tool-{id}"));
            let id_for_click = id.clone();
            let target_id_for_drop = id.clone();
            let target_id_for_hover = id.clone();
            let name = tool.meta().name.clone();
            let description = tool.meta().description.clone();
            let icon = crate::activity_bar::ActivityBar::icon_for_tool(&id);
            let drag = ToolDrag {
                id: id.clone(),
                name: SharedString::from(name.clone()),
                description: SharedString::from(description.clone()),
            };

            let card = v_flex()
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
                .on_drag(drag, |drag: &ToolDrag, _position, _, cx| {
                    cx.new(|_| ToolDragPreview {
                        id: drag.id.clone(),
                        name: drag.name.clone(),
                        description: drag.description.clone(),
                    })
                })
                .drag_over::<ToolDrag>(move |style, drag, _, _| {
                    if drag.id == target_id_for_hover {
                        style
                    } else {
                        style.bg(accent.opacity(0.16)).border_color(accent)
                    }
                })
                .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                    this.complete_drop(&drag.id, &target_id_for_drop, cx);
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
            let (from_x, from_y) =
                reorder_animation_offset(&previous_order, &current_order, &id, columns);
            if from_x == px(0.0) && from_y == px(0.0) {
                card.into_any_element()
            } else {
                Transition::new(Duration::from_millis(360))
                    .ease(ease_in_out_cubic)
                    .slide_x(from_x, px(0.0))
                    .slide_y(from_y, px(0.0))
                    .apply(card, format!("home-tool-reorder-{id}-{layout_revision}"))
                    .into_any_element()
            }
        });

        let mut tool_grid = div()
            .id("home-tool-grid")
            .w_full()
            .max_w(px(TOOL_GRID_WIDTH))
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_start()
            .gap(px(TOOL_CARD_GAP))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ToolDrag>, _, cx| {
                this.handle_drag_move(event, cx);
            }))
            .children(cards);
        if has_tools {
            tool_grid = tool_grid.child(
                div()
                    .id("home-tool-drop-end")
                    .w_full()
                    .h(px(10.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .drag_over::<ToolDrag>(move |style, _, _, _| style.bg(accent.opacity(0.16)))
                    .on_drop(cx.listener(|this, drag: &ToolDrag, _, cx| {
                        this.complete_drop_to_end(&drag.id, cx);
                    })),
            );
        }

        v_flex()
            .size_full()
            .bg(bg)
            .items_center()
            .justify_center()
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
    previous_order: &[String],
    current_order: &[String],
    id: &str,
    columns: usize,
) -> (gpui::Pixels, gpui::Pixels) {
    let Some(previous_index) = previous_order.iter().position(|item| item == id) else {
        return (px(0.0), px(0.0));
    };
    let Some(current_index) = current_order.iter().position(|item| item == id) else {
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
        let previous = vec!["left".to_owned(), "right".to_owned(), "bottom".to_owned()];
        let current = vec!["right".to_owned(), "left".to_owned(), "bottom".to_owned()];

        let right_offset = reorder_animation_offset(&previous, &current, "right", 2);
        assert_eq!(f32::from(right_offset.0), 296.0);
        assert_eq!(f32::from(right_offset.1), 0.0);

        let left_offset = reorder_animation_offset(&previous, &current, "left", 2);
        assert_eq!(f32::from(left_offset.0), -296.0);
        assert_eq!(f32::from(left_offset.1), 0.0);
    }
}
