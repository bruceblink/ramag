use gpui::{ClickEvent, Entity, IntoElement, ParentElement, Styled, div, prelude::*, px};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    input::InputState,
};
use ramag_ui::PointerDropdownMenu as _;

use super::QueryTab;
use crate::views::result_panel::{ResultPanel, RowSearchConversionStatus, RowSearchMode};

pub(super) struct TransactionSavepointState {
    pub(super) transaction_busy: bool,
    pub(super) running: bool,
    pub(super) dml_busy: bool,
    pub(super) pending_cell_edits: bool,
    pub(super) savepoint_count: usize,
    pub(super) latest_savepoint: Option<String>,
    pub(super) max_savepoints: usize,
}

/// Renders savepoint actions and disables them while related work is active.
pub(super) fn transaction_savepoint_controls(
    query_tab: Entity<QueryTab>,
    state: TransactionSavepointState,
    muted: gpui::Hsla,
) -> impl IntoElement {
    let has_latest = state.latest_savepoint.is_some();
    let create_tab = query_tab.clone();
    let rollback_tab = query_tab.clone();
    let release_tab = query_tab.clone();
    h_flex()
        .flex_none()
        .items_center()
        .gap_1()
        .child(
            ramag_ui::clickable_button("transaction-savepoint-create")
                .ghost()
                .small()
                .label("保存点")
                .tooltip("创建保存点")
                .disabled(
                    state.transaction_busy
                        || state.running
                        || state.dml_busy
                        || state.pending_cell_edits
                        || state.savepoint_count >= state.max_savepoints,
                )
                .on_click(move |_: &ClickEvent, _, app| {
                    create_tab.update(app, |tab, cx| tab.create_savepoint(cx));
                }),
        )
        .child(
            ramag_ui::clickable_button("transaction-savepoint-rollback")
                .ghost()
                .small()
                .label("回滚最近")
                .tooltip("回滚到最近的保存点")
                .disabled(
                    state.transaction_busy
                        || state.running
                        || state.dml_busy
                        || state.pending_cell_edits
                        || !has_latest,
                )
                .on_click(move |_: &ClickEvent, _, app| {
                    rollback_tab.update(app, |tab, cx| tab.rollback_to_latest_savepoint(cx));
                }),
        )
        .child(
            ramag_ui::clickable_button("transaction-savepoint-release")
                .ghost()
                .small()
                .label("释放最近")
                .tooltip("释放最近的保存点")
                .disabled(
                    state.transaction_busy
                        || state.running
                        || state.dml_busy
                        || state.pending_cell_edits
                        || !has_latest,
                )
                .on_click(move |_: &ClickEvent, _, app| {
                    release_tab.update(app, |tab, cx| tab.release_latest_savepoint(cx));
                }),
        )
        .when_some(state.latest_savepoint, |controls, name| {
            controls.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("最近 {name}")),
            )
        })
}

pub(super) fn result_view_tabs(
    query_tab: Entity<QueryTab>,
    plan_visible: bool,
    plan_available: bool,
    border: gpui::Hsla,
    secondary_bg: gpui::Hsla,
) -> impl IntoElement {
    h_flex()
        .id("sql-result-view-tabs")
        .debug_selector(|| "sql-result-view-tabs".into())
        .w_full()
        .flex_none()
        .items_center()
        .gap_1()
        .px_2()
        .py(px(3.0))
        .border_b_1()
        .border_color(border)
        .bg(secondary_bg)
        .child(
            ramag_ui::clickable_button("sql-data-result-tab")
                .ghost()
                .small()
                .label("数据结果")
                .when(!plan_visible, |button| button.primary())
                .on_click({
                    let query_tab = query_tab.clone();
                    move |_, _, app| {
                        query_tab.update(app, |tab, cx| {
                            tab.set_plan_visible(false, cx);
                        });
                    }
                }),
        )
        .child(
            ramag_ui::clickable_button("sql-plan-result-tab")
                .ghost()
                .small()
                .label("执行计划")
                .tooltip(if plan_available {
                    "查看最近一次执行计划"
                } else {
                    "先点击工具栏中的执行计划"
                })
                .disabled(!plan_available)
                .when(plan_visible, |button| button.primary())
                .on_click({
                    move |_, _, app| {
                        query_tab.update(app, |tab, cx| {
                            tab.set_plan_visible(true, cx);
                        });
                    }
                }),
        )
        .child(div().flex_1())
}

pub(super) fn row_filter_prefix(
    current: RowSearchMode,
    result: Entity<ResultPanel>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    id_conversion_ready: bool,
) -> gpui::AnyElement {
    if id_conversion_ready {
        row_search_mode_button(current, result, accent).into_any_element()
    } else {
        div()
            .flex_none()
            .text_xs()
            .text_color(muted)
            .child("WHERE")
            .into_any_element()
    }
}

pub(super) fn row_search_mode_button(
    current: RowSearchMode,
    result: Entity<ResultPanel>,
    accent: gpui::Hsla,
) -> impl IntoElement {
    let display_label = match current {
        RowSearchMode::Normal => "WHERE",
        RowSearchMode::IdToInteger => current.label(),
        RowSearchMode::IdToString => current.label(),
    };
    ramag_ui::clickable_button("sql-row-search-mode")
        .text()
        .small()
        // 文本自带显式颜色，避免 Text 按下态短暂继承主题前景色。
        .child(div().flex_none().text_color(accent).child(display_label))
        .dropdown_caret(true)
        .text_color(accent)
        .tooltip(match current {
            RowSearchMode::Normal => "WHERE：按 Enter 将条件发送到数据库执行",
            RowSearchMode::IdToInteger => "@ID -> I：将字符串转为整数，精确匹配整数单元格",
            RowSearchMode::IdToString => "@ID -> S：将非负十进制整数转为字符串，精确匹配文本单元格",
        })
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for mode in RowSearchMode::ALL {
                let result = result.clone();
                menu = menu.item(
                    ramag_ui::menu_item(mode.label())
                        .checked(mode == current)
                        .on_click(move |_: &ClickEvent, _, app| {
                            result.update(app, |panel, cx| {
                                panel.set_row_search_mode(mode, cx);
                            });
                        }),
                );
            }
            menu
        })
}

pub(super) fn row_search_input_suffix(
    input: Entity<InputState>,
    status: Option<RowSearchConversionStatus>,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .when_some(status, |suffix, status| {
            suffix.child(row_search_conversion_label(status, accent, muted, danger))
        })
        .child(
            ramag_ui::clickable_button("sql-row-filter-clear")
                .icon(IconName::CircleX)
                .ghost()
                .xsmall()
                .tab_stop(false)
                .text_color(muted)
                .tooltip("清除")
                .on_click(move |_, window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                }),
        )
}

fn row_search_conversion_label(
    status: RowSearchConversionStatus,
    accent: gpui::Hsla,
    muted: gpui::Hsla,
    danger: gpui::Hsla,
) -> gpui::AnyElement {
    let (label, color) = match status {
        RowSearchConversionStatus::Converting => ("→ 转换中…".to_string(), muted),
        RowSearchConversionStatus::Ready(output) => {
            (format!("→ {}", output.display_preview(40)), accent)
        }
        RowSearchConversionStatus::Error(_) => ("→ 转换失败".to_string(), danger),
    };

    div()
        .flex_none()
        .text_xs()
        .text_color(color)
        .child(label)
        .into_any_element()
}
