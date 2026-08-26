use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Point, ScrollHandle, ScrollStrategy,
    SharedString, Styled, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::ButtonVariants as _,
    h_flex,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use ramag_domain::entities::QueryResult;
use ramag_ui::RestrictScrollToAxisExt as _;

use super::ResultPanel;
use crate::views::result_table::render_table;

#[path = "plan_parser.rs"]
mod parser;
use parser::parse_plan;

const PLAN_CONTENT_WIDTH: f32 = 1_200.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanViewMode {
    Tree,
    Raw,
}

pub(super) struct PlanState {
    pub(super) enabled: bool,
    pub(super) view_mode: PlanViewMode,
    pub(super) tree: Option<PlanTree>,
    pub(super) parse_attempted: bool,
    pub(super) collapsed: BTreeSet<usize>,
    pub(super) vertical_scroll: UniformListScrollHandle,
    pub(super) horizontal_scroll: ScrollHandle,
}

impl PlanState {
    pub(super) fn new() -> Self {
        Self {
            enabled: false,
            view_mode: PlanViewMode::Tree,
            tree: None,
            parse_attempted: false,
            collapsed: BTreeSet::new(),
            vertical_scroll: UniformListScrollHandle::new(),
            horizontal_scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn reset_result(&mut self) {
        self.view_mode = PlanViewMode::Tree;
        self.tree = None;
        self.parse_attempted = false;
        self.collapsed.clear();
        self.vertical_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.horizontal_scroll
            .set_offset(Point::new(px(0.0), px(0.0)));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanSource {
    Mysql,
    Postgres,
}

impl PlanSource {
    fn label(self) -> &'static str {
        match self {
            Self::Mysql => "MySQL",
            Self::Postgres => "PostgreSQL",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PlanTree {
    source: PlanSource,
    rows: Arc<Vec<PlanRow>>,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct PlanRow {
    id: usize,
    parent: Option<usize>,
    depth: usize,
    label: String,
    detail: Option<String>,
    is_detail: bool,
    has_children: bool,
}

impl ResultPanel {
    pub(crate) fn set_plan_mode(&mut self, enabled: bool) {
        self.plan.enabled = enabled;
    }

    pub(super) fn toggle_plan_node(&mut self, node_id: usize, cx: &mut Context<Self>) {
        if !self.plan.collapsed.remove(&node_id) {
            self.plan.collapsed.insert(node_id);
        }
        cx.notify();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_plan(
    panel: &mut ResultPanel,
    result: &Arc<QueryResult>,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    secondary_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted_bg: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let tree = ensure_plan_tree(panel, result);
    match panel.plan.view_mode {
        PlanViewMode::Tree => match tree {
            Some(tree) => render_plan_tree(panel, tree, cx),
            None => render_raw_plan(
                panel,
                result,
                fg,
                muted_fg,
                secondary_bg,
                border,
                muted_bg,
                accent,
                Some("当前 EXPLAIN 返回格式暂不支持结构化显示"),
                false,
                cx,
            ),
        },
        PlanViewMode::Raw => render_raw_plan(
            panel,
            result,
            fg,
            muted_fg,
            secondary_bg,
            border,
            muted_bg,
            accent,
            None,
            tree.is_some(),
            cx,
        ),
    }
}

fn render_plan_tree(
    panel: &mut ResultPanel,
    tree: PlanTree,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let title = format!(
        "结构化执行计划 · {} · {} 个步骤{}",
        tree.source.label(),
        tree.rows.iter().filter(|row| !row.is_detail).count(),
        if tree.truncated {
            "（已限制显示）"
        } else {
            ""
        }
    );
    let toolbar = render_plan_toolbar(panel, title, true, true, cx);
    let scrollbar = cx.theme().scrollbar;
    let visible_indices = Arc::new(visible_plan_indices(&tree.rows, &panel.plan.collapsed));
    let rows = tree.rows.clone();
    let content_width = px(PLAN_CONTENT_WIDTH);
    let body = uniform_list(
        "plan-tree-rows",
        visible_indices.len(),
        cx.processor(move |panel, range: Range<usize>, _window, cx| {
            range
                .map(|index| render_plan_row(panel, &rows[visible_indices[index]], cx))
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&panel.plan.vertical_scroll)
    .w(content_width)
    .flex_1()
    .restrict_scroll_to_axis();

    let scroll_view = div()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id("plan-tree-horizontal-scroll")
                .debug_selector(|| "plan-tree-horizontal-scroll".into())
                .size_full()
                .overflow_x_scroll()
                .track_scroll(&panel.plan.horizontal_scroll)
                .child(v_flex().w(content_width).h_full().child(body)),
        )
        .child(
            div()
                .id("plan-tree-vertical-scrollbar")
                .debug_selector(|| "plan-tree-vertical-scrollbar".into())
                .absolute()
                .top_0()
                .bottom(px(16.0))
                .right_0()
                .w(px(16.0))
                .bg(scrollbar)
                .child(
                    Scrollbar::vertical(&panel.plan.vertical_scroll)
                        .id("plan-tree-vertical-scrollbar-control")
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        )
        .child(
            div()
                .id("plan-tree-horizontal-scrollbar")
                .debug_selector(|| "plan-tree-horizontal-scrollbar".into())
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(16.0))
                .bg(scrollbar)
                .child(
                    Scrollbar::horizontal(&panel.plan.horizontal_scroll)
                        .id("plan-tree-horizontal-scrollbar-control")
                        .scroll_size(gpui::size(content_width, px(16.0)))
                        .scrollbar_show(ScrollbarShow::Always),
                ),
        );

    v_flex()
        .size_full()
        .min_w_0()
        .child(toolbar)
        .child(scroll_view)
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_raw_plan(
    panel: &mut ResultPanel,
    result: &Arc<QueryResult>,
    fg: gpui::Hsla,
    muted_fg: gpui::Hsla,
    secondary_bg: gpui::Hsla,
    border: gpui::Hsla,
    muted_bg: gpui::Hsla,
    accent: gpui::Hsla,
    note: Option<&str>,
    structured_available: bool,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let title = note.unwrap_or("原始 EXPLAIN 结果");
    let toolbar = render_plan_toolbar(panel, title.to_string(), structured_available, false, cx);
    let table = render_table(
        panel,
        result,
        fg,
        muted_fg,
        secondary_bg,
        border,
        muted_bg,
        accent,
        cx,
    );
    v_flex()
        .size_full()
        .min_w_0()
        .child(toolbar)
        .child(div().flex_1().min_h_0().min_w_0().child(table))
        .into_any_element()
}

fn render_plan_toolbar(
    _panel: &ResultPanel,
    title: String,
    structured_available: bool,
    show_tree: bool,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let switch = if show_tree {
        ramag_ui::clickable_button("plan-view-raw")
            .ghost()
            .small()
            .icon(IconName::File)
            .label("原始")
            .tooltip("查看原始 EXPLAIN 结果")
            .on_click(cx.listener(|this, _, _, cx| {
                this.plan.view_mode = PlanViewMode::Raw;
                cx.notify();
            }))
    } else {
        ramag_ui::clickable_button("plan-view-tree")
            .ghost()
            .small()
            .icon(IconName::Network)
            .label("结构化")
            .tooltip(if structured_available {
                "查看结构化执行计划"
            } else {
                "当前 EXPLAIN 格式无法结构化"
            })
            .disabled(!structured_available)
            .on_click(cx.listener(|this, _, _, cx| {
                this.plan.view_mode = PlanViewMode::Tree;
                cx.notify();
            }))
    };

    h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            Icon::new(IconName::Network)
                .small()
                .text_color(theme.accent),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.foreground)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(title),
        )
        .child(switch)
        .into_any_element()
}

fn render_plan_row(
    panel: &mut ResultPanel,
    row: &PlanRow,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let collapsed = panel.plan.collapsed.contains(&row.id);
    let toggle = if row.has_children {
        let node_id = row.id;
        let panel_entity = cx.entity().clone();
        ramag_ui::clickable_button(SharedString::from(format!("plan-toggle-{node_id}")))
            .ghost()
            .xsmall()
            .icon(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .tooltip(if collapsed {
                "展开步骤"
            } else {
                "收起步骤"
            })
            .on_click(move |_, _, app| {
                panel_entity.update(app, |panel, cx| panel.toggle_plan_node(node_id, cx));
            })
            .into_any_element()
    } else {
        div().w(px(24.0)).flex_none().into_any_element()
    };
    let indentation = px(8.0 + row.depth.min(16) as f32 * 20.0);
    let text_color = if row.is_detail {
        theme.muted_foreground
    } else {
        theme.foreground
    };
    let text = row.detail.as_ref().map_or_else(
        || row.label.clone(),
        |detail| format!("{}  {}", row.label, detail),
    );

    h_flex()
        .id(SharedString::from(format!("plan-row-{}", row.id)))
        .w(px(PLAN_CONTENT_WIDTH))
        .h(px(32.0))
        .flex_none()
        .items_center()
        .gap_1()
        .pl(indentation)
        .pr_2()
        .border_b_1()
        .border_color(theme.border)
        .hover(|this| this.bg(theme.muted))
        .child(toggle)
        .child(
            div()
                .text_xs()
                .text_color(text_color)
                .whitespace_nowrap()
                .child(text),
        )
        .into_any_element()
}

fn ensure_plan_tree(panel: &mut ResultPanel, result: &QueryResult) -> Option<PlanTree> {
    if !panel.plan.parse_attempted {
        panel.plan.parse_attempted = true;
        panel.plan.tree = parse_plan(result);
    }
    panel.plan.tree.clone()
}

fn visible_plan_indices(rows: &[PlanRow], collapsed: &BTreeSet<usize>) -> Vec<usize> {
    let mut hidden = vec![false; rows.len()];
    let mut visible = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let hidden_by_parent = row
            .parent
            .is_some_and(|parent| hidden[parent] || collapsed.contains(&parent));
        hidden[index] = hidden_by_parent;
        if !hidden_by_parent {
            visible.push(index);
        }
    }
    visible
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
