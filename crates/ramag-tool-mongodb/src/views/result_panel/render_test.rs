//! MongoDB 结果表 headless 交互回归测试。
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};
use ramag_domain::entities::MongoQueryResult;
use serde_json::{Map, Value};

use super::{ResultPanel, RowFilter, RowSearchMode, RowViewCache, RowViewKey};

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

    let position = cx
        .debug_bounds("mongo-table-h-scroll")
        .expect("MongoDB result table should be rendered")
        .center();
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
        },
    ));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("mongo-table-h-scrollbar").is_some(),
        "重新开启设置后 MongoDB 结果表应恢复水平滚动条"
    );
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
