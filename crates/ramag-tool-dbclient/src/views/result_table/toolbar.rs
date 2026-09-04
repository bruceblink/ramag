use std::sync::Arc;

use gpui::{AnyElement, Context, IntoElement, ParentElement as _, div, prelude::*};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _,
    v_flex,
};
use ramag_domain::entities::QueryResult;
use ramag_ui::PointerDropdownMenu as _;

use crate::views::result_panel::ResultPanel;
use crate::views::result_value::CellCopyFormat;

/// 结果区域固定使用表格渲染；工具栏只保留表格状态和当前单元格操作。
pub(in crate::views) fn render_result_view(
    panel: &mut ResultPanel,
    result: &Arc<QueryResult>,
    cx: &mut Context<ResultPanel>,
) -> AnyElement {
    let theme = cx.theme();
    let body = super::render_table(
        panel,
        result,
        theme.foreground,
        theme.muted_foreground,
        theme.secondary,
        theme.border,
        theme.muted,
        theme.accent,
        cx,
    );

    if result.columns.is_empty() {
        return body;
    }

    v_flex()
        .size_full()
        .min_w_0()
        .child(render_result_toolbar(panel, cx))
        .child(div().flex_1().min_h_0().min_w_0().child(body))
        .into_any_element()
}

fn render_result_toolbar(panel: &ResultPanel, cx: &mut Context<ResultPanel>) -> AnyElement {
    let value_actions = render_result_value_actions(panel, cx);
    let theme = cx.theme();
    ramag_ui::responsive_toolbar()
        .id("result-view-toolbar")
        .debug_selector(|| "result-view-toolbar".into())
        .flex_none()
        .gap_2()
        .px_3()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            Icon::new(IconName::LayoutDashboard)
                .small()
                .text_color(theme.accent),
        )
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
                .child("结果视图"),
        )
        .child(
            div()
                .id("result-view-indicator")
                .debug_selector(|| "result-view-indicator".into())
                .flex_none()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(theme.foreground)
                .whitespace_nowrap()
                .bg(theme.accent.opacity(0.16))
                .child("表格"),
        )
        .child(value_actions)
        .child(
            div()
                .debug_selector(|| "result-view-toolbar-help".into())
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child("按列查看并编辑结果"),
        )
        .into_any_element()
}

fn render_result_value_actions(panel: &ResultPanel, cx: &mut Context<ResultPanel>) -> AnyElement {
    let panel_entity = cx.entity();
    let has_selection = panel.selected_cell().is_some();
    let trigger = ramag_ui::clickable_button("result-value-actions")
        .debug_selector(|| "result-view-value-actions".into())
        .ghost()
        .small()
        .icon(IconName::Copy)
        .label("复制")
        .dropdown_caret(true)
        .disabled(!has_selection);
    trigger
        .pointer_dropdown_menu(move |mut menu, _, _| {
            for format in CellCopyFormat::ALL {
                let panel = panel_entity.clone();
                menu = menu.item(
                    ramag_ui::menu_item(format!("复制为 {}", format.label()))
                        .icon(IconName::Copy)
                        .on_click(move |_, _, app| {
                            panel.update(app, |panel, cx| {
                                panel.copy_selected_cell_as(format, cx);
                            });
                        }),
                );
            }
            let panel = panel_entity.clone();
            menu.separator()
                .item(ramag_ui::menu_item("查看值").icon(IconName::Eye).on_click(
                    move |_, window, app| {
                        panel.update(app, |panel, cx| {
                            panel.open_selected_cell_viewer(window, cx);
                        });
                    },
                ))
        })
        .into_any_element()
}
