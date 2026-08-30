use gpui::{App, Context, Global, IntoElement, ParentElement, Render, Styled, Window, div, px};
use gpui_component::{h_flex, v_flex};
use ramag_app::ToolRegistry;

const ITEM_HEIGHT: f32 = 40.0;
pub(crate) const ACTIVITY_ITEM_GAP: f32 = 4.0;

/// 拖拽工具入口时在首页和侧栏之间传递的最小数据。
#[derive(Debug, Clone)]
pub(crate) struct ToolDrag {
    pub(crate) id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolDragSurface {
    Home,
    ActivityBar,
}

/// 插入线相对于目标入口的方向；目标索引仍指向拖拽时看到的原入口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolDropSide {
    Left,
    Right,
    Top,
    Bottom,
}

/// 首页网格的固定尺寸和当前列数，供落点计算与实际布局共用。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HomeDropLayout {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) item_count: usize,
    pub(crate) columns: usize,
    pub(crate) gap: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolDropTarget {
    pub(crate) surface: ToolDragSurface,
    pub(crate) index: usize,
    pub(crate) side: ToolDropSide,
}

/// 保存一次拖拽的临时来源和插入线目标，不直接改变持久化的工具顺序。
#[derive(Clone, Debug, Default)]
pub(crate) struct ToolDragGlobal {
    pub(crate) dragged_id: Option<String>,
    pub(crate) source_index: usize,
    pub(crate) target: Option<ToolDropTarget>,
    pub(crate) revision: u64,
}

impl Global for ToolDragGlobal {}

/// 初始化拖拽状态，并让来源位置立即成为当前插入线目标。
pub(crate) fn begin_tool_drag(
    dragged_id: &str,
    surface: ToolDragSurface,
    source_index: usize,
    cx: &mut App,
) {
    let revision = cx
        .try_global::<ToolDragGlobal>()
        .map_or(1, |state| state.revision.wrapping_add(1));
    cx.set_global(ToolDragGlobal {
        dragged_id: Some(dragged_id.to_owned()),
        source_index,
        target: Some(ToolDropTarget {
            surface,
            index: source_index,
            side: match surface {
                ToolDragSurface::Home => ToolDropSide::Left,
                ToolDragSurface::ActivityBar => ToolDropSide::Top,
            },
        }),
        revision,
    });
}

/// 更新插入线目标；只有目标入口或目标边缘变化时才通知两个视图重绘。
pub(crate) fn update_tool_drop_target(
    surface: ToolDragSurface,
    index: usize,
    side: ToolDropSide,
    cx: &mut App,
) {
    if !cx.has_active_drag() {
        return;
    }
    let Some(current) = cx.try_global::<ToolDragGlobal>() else {
        return;
    };
    if current.dragged_id.is_none()
        || current.target.as_ref().is_some_and(|target| {
            target.surface == surface && target.index == index && target.side == side
        })
    {
        return;
    }

    let mut next = current.clone();
    next.target = Some(ToolDropTarget {
        surface,
        index,
        side,
    });
    next.revision = next.revision.wrapping_add(1);
    cx.set_global(next);
}

/// 清除拖拽状态，保证放弃拖动后两个视图都移除插入线。
pub(crate) fn clear_tool_drag(cx: &mut App) {
    let Some(current) = cx.try_global::<ToolDragGlobal>() else {
        return;
    };
    if current.dragged_id.is_none() {
        return;
    }

    let revision = current.revision.wrapping_add(1);
    cx.set_global(ToolDragGlobal {
        revision,
        ..ToolDragGlobal::default()
    });
}

/// 读取当前拖拽快照；没有活动拖拽时返回空状态。
pub(crate) fn tool_drag_state(cx: &App) -> ToolDragGlobal {
    cx.try_global::<ToolDragGlobal>()
        .cloned()
        .unwrap_or_default()
}

/// 将目标入口和边缘转换为注册表删除拖拽项后的最终可见索引。
pub(crate) fn tool_drop_index(surface: ToolDragSurface, fallback: usize, cx: &App) -> usize {
    let state = tool_drag_state(cx);
    let Some(target) = state.target.filter(|target| target.surface == surface) else {
        return fallback;
    };

    let boundary = tool_drop_boundary(target.index, target.side);
    boundary.saturating_sub(if boundary > state.source_index { 1 } else { 0 })
}

/// 将目标卡片的四个边缘转换为原始列表中的插入边界。
pub(crate) fn tool_drop_boundary(target_index: usize, side: ToolDropSide) -> usize {
    match side {
        ToolDropSide::Left | ToolDropSide::Top => target_index,
        ToolDropSide::Right | ToolDropSide::Bottom => target_index.saturating_add(1),
    }
}

/// 根据指针在卡片中的位置选择四条边之一，中心位置沿拖拽方向决定左右边。
pub(crate) fn home_drop_side(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    source_index: usize,
    target_index: usize,
) -> ToolDropSide {
    let half_width = width / 2.0;
    let half_height = height / 2.0;
    let horizontal_distance = (x - half_width).abs() / width.max(1.0);
    let vertical_distance = (y - half_height).abs() / height.max(1.0);
    if horizontal_distance >= vertical_distance {
        if x < half_width || (x == half_width && source_index > target_index) {
            ToolDropSide::Left
        } else {
            ToolDropSide::Right
        }
    } else if y < half_height {
        ToolDropSide::Top
    } else {
        ToolDropSide::Bottom
    }
}

/// 根据首页网格中的指针坐标找到卡片和边缘，间隙直接对应前一张卡片的后侧。
pub(crate) fn home_drop_target_from_position(
    x: f32,
    y: f32,
    source_index: usize,
    layout: HomeDropLayout,
) -> Option<(usize, ToolDropSide)> {
    if layout.item_count == 0 || x < 0.0 || y < 0.0 {
        return None;
    }

    let columns = layout.columns.max(1);
    let cell_width = layout.width + layout.gap;
    let cell_height = layout.height + layout.gap;
    let column = (x / cell_width).floor() as usize;
    let row = (y / cell_height).floor() as usize;
    if column >= columns {
        return None;
    }

    let target_index = row.saturating_mul(columns).saturating_add(column);
    if target_index >= layout.item_count {
        return Some((layout.item_count - 1, ToolDropSide::Bottom));
    }

    let local_x = x - column as f32 * cell_width;
    let local_y = y - row as f32 * cell_height;
    let side = if local_x > layout.width {
        ToolDropSide::Right
    } else if local_y > layout.height {
        ToolDropSide::Bottom
    } else {
        home_drop_side(
            local_x,
            local_y,
            layout.width,
            layout.height,
            source_index,
            target_index,
        )
    };
    Some((target_index, side))
}

/// 根据侧栏中的指针坐标找到工具入口或末尾落点，只返回水平线所需的上下边。
pub(crate) fn activity_drop_target_from_position(
    y: f32,
    item_count: usize,
) -> Option<(usize, ToolDropSide)> {
    if item_count == 0 || y < 0.0 {
        return None;
    }

    let slot_height = ITEM_HEIGHT + ACTIVITY_ITEM_GAP;
    let target_index = (y / slot_height).floor() as usize;
    if target_index >= item_count {
        return Some((item_count - 1, ToolDropSide::Bottom));
    }

    let local_y = y - target_index as f32 * slot_height;
    let side = if local_y > ITEM_HEIGHT {
        ToolDropSide::Bottom
    } else if local_y < ITEM_HEIGHT / 2.0 {
        ToolDropSide::Top
    } else {
        ToolDropSide::Bottom
    };
    Some((target_index, side))
}

/// 返回拖拽过程中的实际入口顺序，不改变卡片或菜单项的布局尺寸。
///
/// 拖拽预览是零尺寸的，因此真实入口不能被从列表中移除；插入位置由
/// 单独的 2px 指示线表达，避免线条占用某个标签的完整槽位。
pub(crate) fn tool_drag_display_slots(
    ids: &[String],
    _surface: ToolDragSurface,
    _state: &ToolDragGlobal,
) -> Vec<Option<String>> {
    ids.iter().cloned().map(Some).collect()
}

/// 工具顺序变化通知；注册表保存顺序，Global 只负责让各个视图重绘。
#[derive(Clone, Copy, Default)]
pub(crate) struct ToolLayoutGlobal {
    pub(crate) revision: u64,
}

impl Global for ToolLayoutGlobal {}

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

/// 拖拽只传递排序数据，不绘制跟随指针的卡片；按钮自身仍保留原有 tooltip。
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

/// 计算首页插入线的位置；返回的坐标位于卡片间隙，不参与网格排版。
pub(crate) fn home_drop_indicator(
    accent: gpui::Hsla,
    source_index: usize,
    target_index: usize,
    side: ToolDropSide,
    layout: HomeDropLayout,
) -> Option<gpui::Div> {
    let (line_width, line_height, left, top) =
        home_drop_indicator_geometry(source_index, target_index, side, layout)?;

    Some(
        div()
            .absolute()
            .w(px(line_width))
            .h(px(line_height))
            .left(px(left))
            .top(px(top))
            .rounded(px(1.0))
            .bg(accent.opacity(0.82)),
    )
}

/// 计算首页四种边缘指示线的几何位置，并隐藏拖回原位置的无效落点。
fn home_drop_indicator_geometry(
    source_index: usize,
    target_index: usize,
    side: ToolDropSide,
    layout: HomeDropLayout,
) -> Option<(f32, f32, f32, f32)> {
    if layout.item_count == 0 || target_index >= layout.item_count {
        return None;
    }

    let boundary = tool_drop_boundary(target_index, side);
    if boundary == source_index || boundary == source_index.saturating_add(1) {
        return None;
    }

    let width = layout.width;
    let height = layout.height;
    let gap = layout.gap;
    let columns = layout.columns.max(1);
    let anchor_index = target_index;
    let column = anchor_index % columns;
    let row = anchor_index / columns;
    let cell_width = width + gap;
    let cell_height = height + gap;
    Some(match side {
        ToolDropSide::Left => (
            2.0,
            height,
            if column == 0 {
                0.0
            } else {
                column as f32 * cell_width - gap / 2.0
            },
            row as f32 * cell_height,
        ),
        ToolDropSide::Right => (
            2.0,
            height,
            column as f32 * cell_width
                + if column + 1 < columns && target_index + 1 < layout.item_count {
                    width + gap / 2.0
                } else {
                    width
                },
            row as f32 * cell_height,
        ),
        ToolDropSide::Top => (
            width,
            2.0,
            column as f32 * cell_width,
            if row == 0 {
                0.0
            } else {
                row as f32 * cell_height - gap / 2.0
            },
        ),
        ToolDropSide::Bottom => (
            width,
            2.0,
            column as f32 * cell_width,
            row as f32 * cell_height
                + if row + 1 < layout.item_count.div_ceil(columns) {
                    height + gap / 2.0
                } else {
                    height
                },
        ),
    })
}

/// 计算侧栏插入线的位置；线条只落在相邻工具入口的间隙中。
pub(crate) fn activity_drop_indicator(
    accent: gpui::Hsla,
    source_index: usize,
    target_index: usize,
    side: ToolDropSide,
    item_count: usize,
) -> Option<gpui::Div> {
    let boundary = tool_drop_boundary(target_index, side);
    if item_count == 0
        || target_index >= item_count
        || boundary == source_index
        || boundary == source_index.saturating_add(1)
    {
        return None;
    }

    let top = activity_drop_line_top(boundary);

    Some(
        div()
            .absolute()
            .w(px(28.0))
            .h(px(2.0))
            .left(px(10.0))
            .top(px(top))
            .rounded(px(1.0))
            .bg(accent.opacity(0.82)),
    )
}

/// 将侧栏插入边界放在两个菜单项之间；列表首尾边界贴住对应外侧边缘。
fn activity_drop_line_top(boundary: usize) -> f32 {
    (boundary as f32 * (ITEM_HEIGHT + ACTIVITY_ITEM_GAP) - ACTIVITY_ITEM_GAP / 2.0).max(0.0)
}

/// 计算侧栏工具从旧槽位移动到新槽位时的垂直起点。
pub(crate) fn activity_reorder_animation_offset(
    previous_slots: &[Option<String>],
    current_slots: &[Option<String>],
    id: &str,
) -> gpui::Pixels {
    let Some(previous_index) = previous_slots
        .iter()
        .position(|slot| slot.as_deref() == Some(id))
    else {
        return px(0.0);
    };
    let Some(current_index) = current_slots
        .iter()
        .position(|slot| slot.as_deref() == Some(id))
    else {
        return px(0.0);
    };
    px((previous_index as f32 - current_index as f32) * (ITEM_HEIGHT + ACTIVITY_ITEM_GAP))
}

#[cfg(test)]
mod tests {
    use super::{
        ToolDragGlobal, ToolDragSurface, ToolDropSide, ToolDropTarget,
        activity_drop_target_from_position, home_drop_side, home_drop_target_from_position,
        tool_drag_display_slots, tool_drop_boundary,
    };

    #[test]
    fn drop_boundaries_follow_the_requested_edge() {
        assert_eq!(tool_drop_boundary(2, ToolDropSide::Left), 2);
        assert_eq!(tool_drop_boundary(2, ToolDropSide::Top), 2);
        assert_eq!(tool_drop_boundary(2, ToolDropSide::Right), 3);
        assert_eq!(tool_drop_boundary(2, ToolDropSide::Bottom), 3);
    }

    #[test]
    fn home_target_selection_supports_all_four_edges() {
        assert_eq!(
            home_drop_side(20.0, 56.0, 280.0, 112.0, 3, 1),
            ToolDropSide::Left
        );
        assert_eq!(
            home_drop_side(260.0, 56.0, 280.0, 112.0, 0, 1),
            ToolDropSide::Right
        );
        assert_eq!(
            home_drop_side(140.0, 10.0, 280.0, 112.0, 3, 1),
            ToolDropSide::Top
        );
        assert_eq!(
            home_drop_side(140.0, 102.0, 280.0, 112.0, 0, 1),
            ToolDropSide::Bottom
        );
    }

    #[test]
    fn home_target_selection_uses_the_gap_after_a_card() {
        assert_eq!(
            home_drop_target_from_position(
                288.0,
                56.0,
                3,
                super::HomeDropLayout {
                    width: 280.0,
                    height: 112.0,
                    item_count: 4,
                    columns: 3,
                    gap: 16.0,
                },
            ),
            Some((0, ToolDropSide::Right))
        );
    }

    #[test]
    fn activity_target_selection_only_uses_horizontal_lines() {
        assert_eq!(
            activity_drop_target_from_position(60.0, 3),
            Some((1, ToolDropSide::Top))
        );
        assert_eq!(
            activity_drop_target_from_position(86.0, 3),
            Some((1, ToolDropSide::Bottom))
        );
    }

    #[test]
    fn display_slots_keep_every_card_while_dragging() {
        let ids = ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let state = ToolDragGlobal {
            dragged_id: Some("b".to_owned()),
            source_index: 1,
            target: Some(ToolDropTarget {
                surface: ToolDragSurface::Home,
                index: 2,
                side: ToolDropSide::Right,
            }),
            revision: 1,
        };

        assert_eq!(
            tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
            vec![
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned()),
                Some("d".to_owned())
            ]
        );
    }

    #[test]
    fn display_slots_keep_the_source_card_during_cross_surface_drag() {
        let ids = ["a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let state = ToolDragGlobal {
            dragged_id: Some("b".to_owned()),
            source_index: 1,
            target: Some(ToolDropTarget {
                surface: ToolDragSurface::ActivityBar,
                index: 0,
                side: ToolDropSide::Top,
            }),
            revision: 1,
        };

        assert_eq!(
            tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
            vec![
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned())
            ]
        );
        assert_eq!(
            tool_drag_display_slots(&ids, ToolDragSurface::ActivityBar, &state),
            vec![
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned())
            ]
        );
    }

    #[test]
    fn display_slots_keep_the_source_card_before_the_first_move() {
        let ids = ["a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let state = ToolDragGlobal {
            dragged_id: Some("b".to_owned()),
            source_index: 1,
            target: Some(ToolDropTarget {
                surface: ToolDragSurface::Home,
                index: 1,
                side: ToolDropSide::Left,
            }),
            revision: 1,
        };

        assert_eq!(
            tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
            vec![
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned())
            ]
        );
    }

    #[test]
    fn display_slots_keep_every_card_when_moving_to_an_earlier_target() {
        let ids = ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let state = ToolDragGlobal {
            dragged_id: Some("d".to_owned()),
            source_index: 3,
            target: Some(ToolDropTarget {
                surface: ToolDragSurface::Home,
                index: 1,
                side: ToolDropSide::Left,
            }),
            revision: 1,
        };

        assert_eq!(
            tool_drag_display_slots(&ids, ToolDragSurface::Home, &state),
            vec![
                Some("a".to_owned()),
                Some("b".to_owned()),
                Some("c".to_owned()),
                Some("d".to_owned())
            ]
        );
    }
}
