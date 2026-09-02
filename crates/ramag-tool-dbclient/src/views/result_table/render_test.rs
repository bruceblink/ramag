#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, TestAppContext, TouchPhase, Window, div, point, px,
};
use ramag_domain::entities::{QueryResult, Row, Value};

use super::{DisplayViewCache, DisplayViewCacheKey, build_display_view, cached_display_view};
use crate::views::result_panel::{
    ResultPagination, ResultPanel, ResultState, ResultViewMode, TotalRows,
};

/// 测试宿主同时渲染结果面板和 GPUI Component 的对话框浮层。
struct ResultDialogTestHost {
    panel: Entity<ResultPanel>,
}

impl Render for ResultDialogTestHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        div()
            .relative()
            .size_full()
            .child(self.panel.clone())
            .children(dialog_layer)
    }
}

/// Windows 触控板会同时上报少量另一轴位移；横向浏览列时不能带着行上下移动。
#[gpui::test]
fn result_scroll_horizontal_gesture_does_not_move_rows_vertically(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let column_count = 16;
    let row_count = 80;
    let result = Arc::new(QueryResult {
        columns: (0..column_count)
            .map(|index| format!("column_{index}"))
            .collect(),
        column_types: vec!["TEXT".into(); column_count],
        rows: (0..row_count)
            .map(|row_index| Row {
                values: (0..column_count)
                    .map(|column_index| {
                        Value::Text(format!("row-{row_index}-column-{column_index}"))
                    })
                    .collect(),
            })
            .collect(),
        affected_rows: 0,
        elapsed_ms: 1,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel.state = ResultState::Ok(result);
        panel.display_view_cache = Some(DisplayViewCache {
            key: display_view_key,
            view: display_view,
        });
        panel
    });
    panel.read_with(cx, |panel, cx| {
        let ResultState::Ok(result) = &panel.state else {
            panic!("result state should be ready");
        };
        assert!(
            cached_display_view(panel, result, cx).is_some(),
            "injected display view should match the result"
        );
    });
    panel.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds("result-h-scroll")
        .expect("result table should be rendered");
    assert!(
        cx.debug_bounds("result-v-scrollbar").is_some(),
        "result table should expose a draggable vertical scrollbar"
    );
    // Keep the gesture away from the fixed vertical scrollbar rail at the right edge.
    let position = point(bounds.origin.x + px(80.0), bounds.origin.y + px(80.0));
    assert!(
        cx.debug_bounds("result-h-scrollbar").is_some(),
        "result table should expose a draggable horizontal scrollbar"
    );
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(-80.0), px(-8.0))),
        touch_phase: TouchPhase::Moved,
        ..Default::default()
    });

    panel.read_with(cx, |panel, _| {
        let horizontal = panel.h_scroll.offset();
        let vertical = panel.uniform_scroll.0.borrow().base_handle.offset();
        assert!(horizontal.x < px(0.0), "横向手势应移动结果列");
        assert_eq!(vertical.y, px(0.0), "横向手势不应移动结果行");
    });

    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: false,
            display_binary_16_as_uuid: true,
        },
    ));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("result-h-scrollbar").is_none(),
        "关闭设置后 SQL 结果表不应渲染水平滚动条"
    );

    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        },
    ));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("result-h-scrollbar").is_some(),
        "重新开启设置后 SQL 结果表应恢复水平滚动条"
    );
}

/// Result display modes should switch local renderers without replacing the loaded result.
#[gpui::test]
fn result_view_modes_keep_loaded_selection_and_render_each_surface(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        },
    ));
    let result = Arc::new(QueryResult {
        columns: vec!["id".into(), "name".into()],
        column_types: vec!["BIGINT".into(), "TEXT".into()],
        rows: vec![
            Row {
                values: vec![Value::Int(1), Value::Text("alpha".into())],
            },
            Row {
                values: vec![Value::Int(2), Value::Text("beta".into())],
            },
        ],
        affected_rows: 0,
        elapsed_ms: 1,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel.state = ResultState::Ok(result.clone());
        panel.selected_cell = Some((1, 1));
        panel.display_view_cache = Some(DisplayViewCache {
            key: display_view_key,
            view: display_view,
        });
        panel
    });
    panel.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    for (selector, mode, surface) in [
        (
            "result-view-mode-tree",
            ResultViewMode::Tree,
            "result-tree-scroll",
        ),
        (
            "result-view-mode-text",
            ResultViewMode::Text,
            "result-text-scroll",
        ),
        (
            "result-view-mode-transpose",
            ResultViewMode::Transpose,
            "result-transpose-scroll",
        ),
        (
            "result-view-mode-table",
            ResultViewMode::Table,
            "result-h-scroll",
        ),
    ] {
        let button = cx
            .debug_bounds(selector)
            .expect("result view mode button should be rendered");
        cx.simulate_click(button.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds(surface).is_some(),
            "selected result mode should render its surface"
        );
        panel.read_with(cx, |panel, _cx| {
            assert_eq!(panel.view_mode(), mode);
            assert_eq!(panel.selected_cell(), Some((1, 1)));
            let ResultState::Ok(current) = panel.state() else {
                panic!("result mode switching should keep the loaded result");
            };
            assert!(Arc::ptr_eq(current, &result));
        });
    }

    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: false,
            display_binary_16_as_uuid: true,
        },
    ));
    cx.run_until_parked();
    let text_button = cx
        .debug_bounds("result-view-mode-text")
        .expect("text mode button should remain available");
    cx.simulate_click(text_button.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("result-text-h-scrollbar").is_none(),
        "alternate result views should honor the global horizontal scrollbar setting"
    );
}

/// 长状态文本不能把分页控件推出结果状态栏的可视区域。
#[gpui::test]
fn result_status_keeps_paging_controls_visible_in_small_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        },
    ));
    let result = Arc::new(QueryResult {
        columns: vec!["id".into(), "description".into()],
        column_types: vec!["BIGINT".into(), "TEXT".into()],
        rows: vec![Row {
            values: vec![Value::Int(1), Value::Text("状态信息".repeat(120))],
        }],
        affected_rows: 0,
        elapsed_ms: 123,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel.state = ResultState::Ok(result.clone());
        panel.selected_cell = Some((0, 1));
        panel.selected_rows.insert(0);
        panel.pagination = Some(ResultPagination {
            page: 0,
            page_size: 100,
            has_more: true,
            total: TotalRows::Known(10_000),
        });
        panel.display_view_cache = Some(DisplayViewCache {
            key: display_view_key,
            view: display_view,
        });
        panel
    });
    cx.simulate_resize(gpui::size(px(720.0), px(420.0)));
    panel.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let status_bar = cx
        .debug_bounds("result-status-bar")
        .expect("结果状态栏应渲染");
    let status_context = cx
        .debug_bounds("result-status-context")
        .expect("状态摘要区域应渲染");
    let next_page = cx
        .debug_bounds("result-page-next")
        .expect("下一页按钮应渲染");
    assert!(status_context.size.width > px(0.0));
    assert!(
        next_page.right() <= status_bar.right(),
        "分页按钮不能被长状态文本推出状态栏"
    );
}

/// 结果树、文本和转置视图的状态栏也不能被选中值挤出分页控件。
#[gpui::test]
fn alternate_result_status_keeps_paging_controls_visible_in_small_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        },
    ));
    let result = Arc::new(QueryResult {
        columns: vec!["id".into(), "description_".repeat(40)],
        column_types: vec!["BIGINT".into(), "TEXT".into()],
        rows: vec![Row {
            values: vec![Value::Int(1), Value::Text("状态信息".repeat(120))],
        }],
        affected_rows: 0,
        elapsed_ms: 123,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel.state = ResultState::Ok(result.clone());
        panel.view_mode = ResultViewMode::Tree;
        panel.selected_cell = Some((0, 1));
        panel.pagination = Some(ResultPagination {
            page: 0,
            page_size: 100,
            has_more: true,
            total: TotalRows::Known(10_000),
        });
        panel.display_view_cache = Some(DisplayViewCache {
            key: display_view_key,
            view: display_view,
        });
        panel
    });
    cx.simulate_resize(gpui::size(px(720.0), px(420.0)));
    panel.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let status_bar = cx
        .debug_bounds("result-alternate-status")
        .expect("替代结果视图状态栏应渲染");
    let status_context = cx
        .debug_bounds("result-alternate-status-context")
        .expect("替代结果视图状态摘要区域应渲染");
    let next_page = cx
        .debug_bounds("result-alternate-page-next")
        .expect("替代结果视图下一页按钮应渲染");
    assert!(status_context.size.width > px(0.0));
    assert!(
        next_page.right() <= status_bar.right(),
        "替代结果视图分页按钮不能被长状态文本推出状态栏"
    );
}

/// The value viewer should render only the selected cell and remain closable as a dialog.
#[gpui::test]
fn selected_cell_opens_bounded_value_viewer(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let result = Arc::new(QueryResult {
        columns: vec!["id".into(), "payload".into()],
        column_types: vec!["INT".into(), "TEXT".into()],
        rows: vec![Row {
            values: vec![Value::Int(1), Value::Text("line one\nline two".into())],
        }],
        affected_rows: 0,
        elapsed_ms: 1,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = ResultPanel::new(window, cx);
            panel.state = ResultState::Ok(result.clone());
            panel.selected_cell = Some((0, 1));
            panel.display_view_cache = Some(DisplayViewCache {
                key: display_view_key,
                view: display_view,
            });
            panel
        });
        panel_entity = Some(panel.clone());
        let host = cx.new(|_| ResultDialogTestHost {
            panel: panel.clone(),
        });
        gpui_component::Root::new(host, window, cx)
    });
    let panel = panel_entity.expect("result panel should be initialized");
    cx.run_until_parked();

    panel.update_in(cx, |panel, window, cx| {
        panel.open_selected_cell_viewer(window, cx);
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("result-value-viewer").is_some(),
        "selected cell should open the value viewer"
    );
    assert!(
        cx.debug_bounds("result-value-viewer-content-frame")
            .is_some(),
        "value viewer should render a selectable bounded content area"
    );
    assert!(
        cx.debug_bounds("result-value-viewer-h-scrollbar").is_some(),
        "value viewer should provide horizontal scrolling for long lines"
    );

    let close = cx
        .debug_bounds("result-value-viewer-close")
        .expect("value viewer should expose a close action");
    cx.simulate_click(close.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("result-value-viewer").is_none(),
        "closing the value viewer should remove the dialog"
    );
}

#[gpui::test]
fn clearing_table_filter_preserves_content_search(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel
            .column_filter_entity()
            .update(cx, |state, cx| state.set_value("metadata", window, cx));
        panel
            .row_filter_entity()
            .update(cx, |state, cx| state.set_value("0", window, cx));
        panel.clear_column_filter(window, cx);
        panel
    });

    panel.read_with(cx, |panel, cx| {
        assert!(panel.column_filter_entity().read(cx).value().is_empty());
        assert_eq!(panel.row_filter_entity().read(cx).value().as_ref(), "0");
    });
}

/// 可写单元格进入行内编辑后应保留源坐标和初始值，并能完整取消。
#[gpui::test]
fn inline_cell_edit_keeps_target_and_clears_on_cancel(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let result = Arc::new(QueryResult {
        columns: vec!["id".into(), "status".into()],
        column_types: vec!["INT".into(), "TEXT".into()],
        rows: vec![Row {
            values: vec![Value::Int(1), Value::Text("pending".into())],
        }],
        affected_rows: 0,
        elapsed_ms: 1,
        warnings: Vec::new(),
        truncated: false,
    });
    let display_view = build_display_view(&result, None, "", "");
    let display_view_key = DisplayViewCacheKey {
        result_identity: Arc::as_ptr(&result) as usize,
        result_revision: 0,
        sort_by: None,
        column_filter: String::new(),
        row_filter: super::RowFilter::Text(String::new()),
        display_binary_16_as_uuid: true,
    };
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = ResultPanel::new(window, cx);
            panel.state = ResultState::Ok(result);
            panel.display_view_cache = Some(DisplayViewCache {
                key: display_view_key,
                view: display_view,
            });
            panel
        });
        panel_entity = Some(panel.clone());
        gpui_component::Root::new(panel, window, cx)
    });
    let panel = panel_entity.expect("result panel should be initialized");
    cx.run_until_parked();
    panel.update_in(cx, |panel, window, cx| {
        panel.begin_cell_edit(0, 1, "pending".into(), window, cx);
    });

    panel.read_with(cx, |panel, cx| {
        assert_eq!(panel.editing_cell, Some((0, 1)));
        let input = panel
            .cell_edit_input
            .as_ref()
            .expect("inline editor should be allocated");
        assert_eq!(input.read(cx).value().as_ref(), "pending");
    });

    panel.update(cx, |panel, cx| panel.cancel_inline_cell_edit(cx));
    panel.read_with(cx, |panel, _| {
        assert!(panel.editing_cell.is_none());
        assert!(panel.cell_edit_input.is_none());
        assert!(panel.cell_edit_subscription.is_none());
    });
}
