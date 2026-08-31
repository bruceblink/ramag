use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, TestAppContext, Window, div, px, size,
};
use gpui_component::{ActiveTheme as _, Root, v_flex};

use super::core_history::{
    CORE_HISTORY_HEIGHT, CORE_HISTORY_MIN_BAR_HEIGHT, CORE_HISTORY_MIN_BAR_RATIO,
    core_histogram_bar_ratio,
};
use super::{
    HISTORY_CHART_POINTS, chart_value_ratio, core_grid_dimensions, format_bytes, format_percent,
    format_rate_pair, history_chart_points, history_max, ratio_percent, render_core_grid,
};

struct CoreGridTestHost;

impl Render for CoreGridTestHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let usages = (0..128)
            .map(|index| (index % 100) as f32)
            .collect::<Vec<_>>();
        let histories = vec![Vec::new(); usages.len()];
        v_flex().size_full().child(
            v_flex()
                .debug_selector(|| "system-core-panel".to_owned())
                .w_full()
                .h(px(260.0))
                .gap(px(6.0))
                .p(px(12.0))
                .child(div().h(px(20.0)).flex_none())
                .child(render_core_grid(&usages, &histories, cx.theme())),
        )
    }
}

struct CoreHistogramTestHost;

impl Render for CoreHistogramTestHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let usages = (0..24)
            .map(|index| if index == 0 { 0.0 } else { 50.0 })
            .collect::<Vec<_>>();
        let mut histories = vec![Vec::<[f64; 2]>::new(); usages.len()];
        histories[0] = vec![[0.0, 0.0], [1.0, 25.0], [2.0, 100.0]];
        v_flex().size_full().child(
            v_flex()
                .debug_selector(|| "system-core-histogram-panel".to_owned())
                .w_full()
                .h(px(260.0))
                .gap(px(6.0))
                .p(px(12.0))
                .child(div().h(px(20.0)).flex_none())
                .child(render_core_grid(&usages, &histories, cx.theme())),
        )
    }
}

#[test]
fn formatters_keep_units_and_bounds() {
    assert_eq!(format_bytes(1024), "1.0 KiB");
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_percent(150.0), "100.0%");
    assert_eq!(ratio_percent(5, 10), 50.0);
    assert_eq!(history_max(&[[0.0, 2.0], [1.0, 8.0]]), 8.0);
}

#[test]
fn history_chart_keeps_the_latest_samples_in_time_order() {
    let points = (0..=HISTORY_CHART_POINTS + 2)
        .map(|value| [value as f64, value as f64])
        .collect::<Vec<_>>();
    let chart_points = history_chart_points(&points);

    assert_eq!(chart_points.len(), HISTORY_CHART_POINTS);
    assert_eq!(chart_points.first(), Some(&[3.0, 3.0]));
    assert_eq!(
        chart_points.last(),
        Some(&[HISTORY_CHART_POINTS as f64 + 2.0; 2])
    );
}

#[test]
fn chart_value_ratio_is_bounded_and_handles_invalid_values() {
    assert_eq!(chart_value_ratio(25.0, 100.0), 0.25);
    assert_eq!(chart_value_ratio(-1.0, 100.0), 0.0);
    assert_eq!(chart_value_ratio(120.0, 100.0), 1.0);
    assert_eq!(chart_value_ratio(f64::NAN, 100.0), 0.0);
    assert_eq!(chart_value_ratio(10.0, 0.0), 0.0);
}

#[test]
fn core_histogram_bar_ratio_keeps_low_values_visible() {
    assert_eq!(core_histogram_bar_ratio(0.0), CORE_HISTORY_MIN_BAR_RATIO);
    assert_eq!(
        core_histogram_bar_ratio(f32::NAN),
        CORE_HISTORY_MIN_BAR_RATIO
    );
    assert_eq!(core_histogram_bar_ratio(100.0), 1.0);
}

#[test]
fn rate_pairs_use_one_shared_unit() {
    assert_eq!(
        format_rate_pair("读", 27.3, "写", 27.0),
        "读 27.3 / 写 27.0 MB/s"
    );
}

#[test]
fn core_grid_keeps_every_core_inside_the_fixed_window() {
    assert_eq!(core_grid_dimensions(0), (1, 1));
    assert_eq!(core_grid_dimensions(1), (1, 1));
    assert_eq!(core_grid_dimensions(12), (4, 3));
    assert_eq!(core_grid_dimensions(33), (9, 4));

    let (columns, rows) = core_grid_dimensions(128);
    assert_eq!((columns, rows), (16, 8));
    assert!(columns * rows >= 128);
}

#[gpui::test]
#[allow(clippy::expect_used)]
fn core_grid_last_tile_is_not_clipped_by_the_fixed_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| CoreGridTestHost);
        Root::new(host, window, cx)
    });
    cx.simulate_resize(size(px(640.0), px(260.0)));
    cx.run_until_parked();

    let grid = cx
        .debug_bounds("system-core-grid")
        .expect("core grid should be rendered");
    let panel = cx
        .debug_bounds("system-core-panel")
        .expect("core panel should be rendered");
    let last_tile = cx
        .debug_bounds("system-core-tile-128")
        .expect("last core tile should be rendered");
    assert!(last_tile.size.width > px(0.0));
    assert!(last_tile.size.height > px(0.0));
    assert!(grid.origin.x >= panel.origin.x);
    assert!(grid.origin.y >= panel.origin.y);
    assert!(grid.origin.x + grid.size.width <= panel.origin.x + panel.size.width);
    assert!(grid.origin.y + grid.size.height <= panel.origin.y + panel.size.height);
    assert!(last_tile.origin.x >= panel.origin.x);
    assert!(last_tile.origin.y >= panel.origin.y);
    assert!(last_tile.origin.x + last_tile.size.width <= panel.origin.x + panel.size.width);
    assert!(last_tile.origin.y + last_tile.size.height <= panel.origin.y + panel.size.height);
    assert!(last_tile.origin.x + last_tile.size.width <= grid.origin.x + grid.size.width);
    assert!(last_tile.origin.y + last_tile.size.height <= grid.origin.y + grid.size.height);
}

#[gpui::test]
#[allow(clippy::expect_used)]
fn core_histogram_renders_visible_bars_inside_each_tile(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| CoreHistogramTestHost);
        Root::new(host, window, cx)
    });
    cx.simulate_resize(size(px(640.0), px(260.0)));
    cx.run_until_parked();

    let graph = cx
        .debug_bounds("system-core-history-1")
        .expect("core histogram should be rendered");
    let tile = cx
        .debug_bounds("system-core-tile-1")
        .expect("core histogram tile should be rendered");
    let low_bar = cx
        .debug_bounds("system-core-bar-1-1")
        .expect("core histogram baseline bar should be rendered");
    let high_bar = cx
        .debug_bounds("system-core-bar-1-3")
        .expect("core histogram high bar should be rendered");
    assert!(graph.size.height >= px(CORE_HISTORY_HEIGHT));
    assert!(
        graph.origin.y >= tile.origin.y,
        "histogram graph should stay inside tile: graph={graph:?}, tile={tile:?}"
    );
    assert!(
        graph.origin.y + graph.size.height <= tile.origin.y + tile.size.height,
        "histogram graph should stay inside tile: graph={graph:?}, tile={tile:?}"
    );
    assert!(low_bar.size.height >= px(CORE_HISTORY_MIN_BAR_HEIGHT));
    assert!(
        high_bar.size.height > low_bar.size.height,
        "high CPU sample should produce a taller bar: graph={graph:?}, low={low_bar:?}, high={high_bar:?}"
    );
}
