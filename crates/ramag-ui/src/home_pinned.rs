use std::sync::Arc;

use gpui::{
    AppContext as _, ClickEvent, Context, DragMoveEvent, InteractiveElement as _, IntoElement,
    ParentElement, StatefulInteractiveElement as _, Styled, div, px,
};
use gpui_component::{Icon, IconName, Sizable, button::ButtonVariants as _, h_flex, v_flex};

use super::{
    HomeEvent, HomeView, TOOL_CARD_GAP, TOOL_CARD_HEIGHT, TOOL_CARD_WIDTH, TOOL_GRID_WIDTH,
};
use crate::tool_layout::{
    HomeDropLayout, ToolDrag, ToolDragPreview, ToolDragSurface, begin_tool_drag,
    dragged_item_background, home_drop_indicator, home_drop_target_from_position, tool_drag_state,
    update_tool_drop_target,
};
use crate::tool_pinning::unpin_tool;

/// 绘制首页固定工具区；固定卡片保留打开、排序和取消固定三个操作。
pub(super) fn render_pinned_tools(
    tools: &[Arc<dyn ramag_domain::Tool>],
    accent: gpui::Hsla,
    border: gpui::Hsla,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    card_bg: gpui::Hsla,
    cx: &mut Context<HomeView>,
) -> impl IntoElement {
    let mut section = v_flex().w_full().max_w(px(TOOL_GRID_WIDTH)).gap(px(12.0));
    if tools.is_empty() {
        return section;
    }
    section = section.child(
        h_flex()
            .w_full()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("已固定"),
            )
            .child(div().text_xs().text_color(muted_fg).child("首页快捷入口")),
    );
    let item_count = tools.len();
    let layout = HomeDropLayout {
        width: TOOL_CARD_WIDTH,
        height: TOOL_CARD_HEIGHT,
        item_count,
        columns: 3,
        gap: TOOL_CARD_GAP,
    };
    let drag_state = tool_drag_state(cx);
    let target = drag_state
        .target
        .as_ref()
        .filter(|target| target.surface == ToolDragSurface::Pinned)
        .map(|target| (target.index, target.side));
    let cards = tools.iter().enumerate().map(|(index, tool)| {
        let id = tool.meta().id.clone();
        let open_id = id.clone();
        let unpin_id = id.clone();
        let name = tool.meta().name.clone();
        let description = tool.meta().description.clone();
        let drag_id = id.clone();
        let preview_name_for_drag = name.clone();
        let preview_description_for_drag = description.clone();
        let is_dragged = drag_state.dragged_id.as_deref() == Some(id.as_str());
        let card_background = if is_dragged {
            dragged_item_background(card_bg)
        } else {
            card_bg
        };
        let card_border = if is_dragged {
            accent.opacity(0.78)
        } else {
            border
        };
        v_flex()
            .id(format!("home-pinned-tool-{id}"))
            .w(px(TOOL_CARD_WIDTH))
            .h(px(TOOL_CARD_HEIGHT))
            .p(px(20.0))
            .gap(px(10.0))
            .bg(card_background)
            .border_1()
            .border_color(card_border)
            .rounded(px(10.0))
            .relative()
            .cursor_pointer()
            .on_drag(ToolDrag { id: drag_id }, move |drag, position, _, cx| {
                begin_tool_drag(&drag.id, ToolDragSurface::Pinned, index, cx);
                cx.new(|_| {
                    ToolDragPreview::new(
                        ToolDragSurface::Pinned,
                        Icon::new(IconName::StarFill),
                        preview_name_for_drag.clone(),
                        preview_description_for_drag.clone(),
                    )
                    .position(position)
                })
            })
            .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                this.complete_pinned_drop(&drag.id, index, cx);
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(Icon::new(IconName::StarFill).text_color(accent))
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
                div().absolute().top(px(10.0)).right(px(10.0)).child(
                    crate::clickable_button(format!("home-tool-unpin-{unpin_id}"))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::StarFill))
                        .tooltip("取消固定")
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            unpin_tool(&unpin_id, cx);
                        })),
                ),
            )
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(HomeEvent::OpenTool(open_id.clone()));
            }))
    });
    let mut grid = div()
        .id("home-pinned-tools")
        .debug_selector(|| "home-pinned-tools".into())
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
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
                    layout,
                ) {
                    update_tool_drop_target(ToolDragSurface::Pinned, target_index, side, cx);
                }
            }),
        )
        .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
            this.complete_pinned_drop(&drag.id, item_count, cx);
        }))
        .children(cards);
    if let Some((target_index, target_side)) = target
        && let Some(indicator) = home_drop_indicator(
            accent,
            drag_state.source_index,
            target_index,
            target_side,
            layout,
        )
    {
        grid = grid.child(
            indicator
                .id("home-pinned-drop-indicator")
                .on_mouse_move(cx.listener(move |_, _, _, cx| {
                    update_tool_drop_target(ToolDragSurface::Pinned, target_index, target_side, cx);
                }))
                .on_drop(cx.listener(move |this, drag: &ToolDrag, _, cx| {
                    this.complete_pinned_drop(&drag.id, target_index, cx);
                })),
        );
    }
    section.child(grid)
}
