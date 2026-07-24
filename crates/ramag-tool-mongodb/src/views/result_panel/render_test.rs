//! MongoDB 结果表 headless 交互回归测试。
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};
use ramag_domain::entities::MongoQueryResult;
use serde_json::{Map, Value};

use super::{ResultPanel, RowViewCache, RowViewKey};

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
                query: String::new(),
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
}
