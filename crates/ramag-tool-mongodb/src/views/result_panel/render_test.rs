//! MongoDB 结果表 headless 交互回归测试。
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px, size};
use ramag_domain::entities::MongoQueryResult;
use serde_json::{Map, Value};

use super::{
    MongoResultPagination, ResultPanel, RowFilter, RowSearchMode, RowViewCache, RowViewKey,
};

fn wide_documents() -> Vec<Value> {
    (0..80)
        .map(|row_index| {
            let fields = (0..16)
                .map(|column_index| {
                    (
                        format!("column_{column_index}"),
                        Value::String(format!("row-{row_index}-column-{column_index}")),
                    )
                })
                .collect::<Map<_, _>>();
            Value::Object(fields)
        })
        .collect()
}

/// Windows 触控板会混入少量另一轴位移；横向浏览列时不能带着行上下移动。
#[gpui::test]
fn result_scroll_horizontal_gesture_does_not_move_rows_vertically(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let documents = wide_documents();
        let table = Arc::new(super::flatten::build_flat_table_with(
            &documents,
            &BTreeSet::new(),
        ));
        let row_indices = Arc::new((0..table.rows.len()).collect());
        let mut panel = ResultPanel::new(window, cx);
        panel.result = Some(MongoQueryResult {
            elapsed_ms: 1,
            ..Default::default()
        });
        panel.docs_arc = Some(Arc::new(documents));
        panel.table_build_seq = 1;
        panel.table = Some(table);
        panel.row_view_cache = Some(RowViewCache {
            key: RowViewKey {
                generation: panel.table_build_seq,
                filter: RowFilter::Text(String::new()),
                sort_by: None,
            },
            indices: row_indices,
        });
        panel
    });
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds("mongo-table-h-scroll")
        .expect("MongoDB result table should be rendered");
    assert!(
        cx.debug_bounds("mongo-table-v-scrollbar").is_some(),
        "MongoDB result table should expose a draggable vertical scrollbar"
    );
    // Keep the gesture away from the fixed vertical scrollbar rail at the right edge.
    let position = point(bounds.origin.x + px(80.0), bounds.origin.y + px(80.0));
    assert!(
        cx.debug_bounds("mongo-table-h-scrollbar").is_some(),
        "MongoDB result table should expose a draggable horizontal scrollbar"
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
        cx.debug_bounds("mongo-table-h-scrollbar").is_none(),
        "关闭设置后 MongoDB 结果表不应渲染水平滚动条"
    );

    cx.set_global(ramag_ui::DatabaseResultSettingsGlobal::new(
        ramag_ui::DatabaseResultSettings {
            show_horizontal_scrollbar: true,
            display_binary_16_as_uuid: true,
        },
    ));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("mongo-table-h-scrollbar").is_some(),
        "重新开启设置后 MongoDB 结果表应恢复水平滚动条"
    );
}

/// 三种窗口下工具栏和摘要不得互相覆盖，分页控件仍需保持在结果状态栏内可见。
#[gpui::test]
fn result_toolbar_and_status_keep_actions_visible_in_three_window_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let documents = wide_documents();
        let table = Arc::new(super::flatten::build_flat_table_with(
            &documents,
            &BTreeSet::new(),
        ));
        let row_indices = Arc::new((0..table.rows.len()).collect());
        let mut panel = ResultPanel::new(window, cx);
        panel.result = Some(MongoQueryResult {
            elapsed_ms: 123,
            ..Default::default()
        });
        panel.docs_arc = Some(Arc::new(documents));
        panel.table_build_seq = 1;
        panel.table = Some(table);
        panel.row_view_cache = Some(RowViewCache {
            key: RowViewKey {
                generation: panel.table_build_seq,
                filter: RowFilter::Text(String::new()),
                sort_by: None,
            },
            indices: row_indices,
        });
        panel.pagination = Some(MongoResultPagination {
            page: 0,
            page_size: 100,
            has_more: true,
        });
        panel
    });
    for (width, height) in [(360.0, 280.0), (1024.0, 420.0), (1440.0, 420.0)] {
        cx.simulate_resize(size(px(width), px(height)));
        panel.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();

        let toolbar = cx
            .debug_bounds("mongo-result-toolbar")
            .expect("MongoDB 结果工具栏应渲染");
        let run = cx
            .debug_bounds("mongo-run-result")
            .expect("MongoDB 运行按钮应渲染");
        let status_bar = cx
            .debug_bounds("mongo-status-bar")
            .expect("MongoDB 结果状态栏应渲染");
        let status_context = cx
            .debug_bounds("mongo-status-context")
            .expect("MongoDB 状态摘要区域应渲染");
        let next_page = cx
            .debug_bounds("mongo-result-page-next")
            .expect("MongoDB 下一页按钮应渲染");

        assert!(toolbar.right() <= px(width));
        assert!(run.right() <= toolbar.right(), "运行按钮不能越出工具栏");
        assert!(status_context.size.width > px(0.0));
        assert!(
            status_bar.origin.y >= toolbar.bottom(),
            "状态栏不能覆盖工具栏"
        );
        assert!(
            next_page.right() <= status_bar.right(),
            "MongoDB 分页按钮不能被状态摘要推出状态栏"
        );
    }
}

#[gpui::test]
fn clearing_layer_filter_preserves_content_search(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel
            .column_filter
            .update(cx, |state, cx| state.set_value("metadata", window, cx));
        panel
            .row_filter
            .update(cx, |state, cx| state.set_value("0", window, cx));
        panel.clear_column_filter(window, cx);
        panel
    });

    panel.read_with(cx, |panel, cx| {
        assert!(panel.column_filter.read(cx).value().is_empty());
        assert_eq!(panel.row_filter.read(cx).value().as_ref(), "0");
    });
}

#[gpui::test]
fn mongodb_id_search_uses_the_shared_converter_configuration(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        ramag_ui::set_database_search_settings(
            ramag_ui::DatabaseSearchSettings {
                id_conversion_enabled: true,
                ..Default::default()
            },
            cx,
        );
    });
    let (panel, cx) = cx.add_window_view(|window, cx| {
        let mut panel = ResultPanel::new(window, cx);
        panel
            .row_filter
            .update(cx, |state, cx| state.set_value("qwe", window, cx));
        panel.set_row_search_mode(RowSearchMode::IdToInteger, cx);
        panel
    });
    cx.run_until_parked();

    panel.read_with(cx, |panel, cx| {
        assert_eq!(panel.effective_row_filter(cx), RowFilter::Integer(82_489));
    });
}
