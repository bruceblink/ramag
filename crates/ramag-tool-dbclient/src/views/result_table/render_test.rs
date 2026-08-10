#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gpui::{ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};
use ramag_domain::entities::{QueryResult, Row, Value};

use super::{DisplayViewCache, DisplayViewCacheKey, build_display_view, cached_display_view};
use crate::views::result_panel::{ResultPanel, ResultState};

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

    let position = cx
        .debug_bounds("result-h-scroll")
        .expect("result table should be rendered")
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
