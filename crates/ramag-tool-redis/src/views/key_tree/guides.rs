//! Redis Key 虚拟树的层级引导线布局。

use gpui::{AnyElement, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use gpui_component::h_flex;

use super::tree::VisibleRow;
use super::{INDENT_PX, NAMESPACE_SEP};

/// 展开的命名空间从箭头中心向下连接第一层子节点。
pub(super) fn render_namespace_stem(
    row_index: usize,
    depth: usize,
    color: gpui::Hsla,
) -> AnyElement {
    div()
        .debug_selector(move || format!("redis-tree-stem-{row_index}"))
        .absolute()
        .left(px(8.0 + depth as f32 * INDENT_PX + INDENT_PX / 2.0))
        .top(px(14.0))
        .bottom_0()
        .border_l_1()
        .border_color(color)
        .into_any_element()
}

/// 为虚拟树行绘制准确的 `├─` / `└─` 层级线；每层固定宽度，不改变缩进总量。
pub(super) fn render_hierarchy_guides(
    row_index: usize,
    depth: usize,
    has_next_sibling: bool,
    ancestor_guide_mask: u16,
    color: gpui::Hsla,
) -> AnyElement {
    h_flex()
        .id(SharedString::from(format!("redis-tree-guides-{row_index}")))
        .debug_selector(move || format!("redis-tree-guides-{row_index}"))
        .h_full()
        .flex_none()
        .children((0..depth).map(move |level| {
            let is_current = level + 1 == depth;
            let draw_vertical = if is_current {
                true
            } else {
                ancestor_guide_mask & (1 << level) != 0
            };
            let mut slot = div().relative().h_full().w(px(INDENT_PX)).flex_none();
            if draw_vertical {
                slot = slot.child(
                    div()
                        .absolute()
                        .left(px(INDENT_PX / 2.0))
                        .top_0()
                        .when(is_current && !has_next_sibling, |line| line.h(px(14.0)))
                        .when(!is_current || has_next_sibling, |line| line.bottom_0())
                        .border_l_1()
                        .border_color(color),
                );
            }
            if is_current {
                slot = slot.child(
                    div()
                        .absolute()
                        .left(px(INDENT_PX / 2.0))
                        .top(px(14.0))
                        .w(px(INDENT_PX / 2.0))
                        .border_t_1()
                        .border_color(color),
                );
            }
            slot
        }))
        .into_any_element()
}

/// 根据最终可见行计算兄弟关系；搜索过滤后也不会把隐藏节点误算为可见分支。
pub(super) fn assign_visible_tree_guides(rows: &mut [VisibleRow]) {
    let mut last_at_depth: Vec<Option<usize>> = Vec::new();
    for index in 0..rows.len() {
        let depth = rows[index].depth;
        last_at_depth.truncate(depth + 1);
        if last_at_depth.len() <= depth {
            last_at_depth.resize(depth + 1, None);
        }
        if let Some(previous) = last_at_depth[depth]
            && visible_parent_path(&rows[previous]) == visible_parent_path(&rows[index])
        {
            rows[previous].has_next_sibling = true;
        }
        last_at_depth[depth] = Some(index);
    }

    let mut ancestors: Vec<usize> = Vec::new();
    for index in 0..rows.len() {
        let depth = rows[index].depth;
        ancestors.truncate(depth);
        let mut mask = 0u16;
        for (ancestor_depth, ancestor) in ancestors.iter().copied().enumerate().skip(1) {
            if rows[ancestor].has_next_sibling {
                mask |= 1 << (ancestor_depth - 1);
            }
        }
        rows[index].ancestor_guide_mask = mask;
        ancestors.push(index);
    }
}

fn visible_parent_path(row: &VisibleRow) -> &str {
    if row.depth == 0 {
        ""
    } else {
        row.full_path
            .rsplit_once(NAMESPACE_SEP)
            .map_or("", |(parent, _)| parent)
    }
}
