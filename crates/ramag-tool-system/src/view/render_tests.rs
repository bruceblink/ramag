use super::*;
use crate::{MonitorSnapshot, SystemMonitor};
use gpui::{AppContext as _, Bounds, Pixels, TestAppContext, px, size};
use gpui_component::Root;

fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
    assert!(
        child.origin.x >= parent.origin.x,
        "{label} starts before parent: parent={parent:?}, child={child:?}"
    );
    assert!(
        child.origin.y >= parent.origin.y,
        "{label} starts above parent: parent={parent:?}, child={child:?}"
    );
    assert!(
        child.origin.x + child.size.width <= parent.origin.x + parent.size.width,
        "{label} exceeds parent horizontally: parent={parent:?}, child={child:?}"
    );
    assert!(
        child.origin.y + child.size.height <= parent.origin.y + parent.size.height,
        "{label} exceeds parent vertically: parent={parent:?}, child={child:?}"
    );
}

/// 在最窄支持宽度渲染真实视图，避免标题栏或任务表在组件边界外被裁切。
#[gpui::test]
#[allow(clippy::expect_used)]
fn narrow_window_keeps_monitor_controls_and_process_table_inside_content(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| SystemView {
            monitor: SystemMonitor::new(),
            section: SystemSection::Processes,
            termination_request: None,
            termination_in_progress: false,
            notice: None,
        });
        Root::new(view, window, cx)
    });
    cx.simulate_resize(size(px(360.0), px(640.0)));
    cx.run_until_parked();

    let header = cx
        .debug_bounds("system-header")
        .expect("system header should be rendered");
    let controls = cx
        .debug_bounds("system-header-controls")
        .expect("system header controls should be rendered");
    let content = cx
        .debug_bounds("system-content")
        .expect("system content should be rendered");
    let table = cx
        .debug_bounds("system-process-table")
        .expect("process table should be rendered");

    assert!(header.size.width <= px(360.0));
    assert!(controls.origin.x >= header.origin.x);
    assert!(controls.origin.x + controls.size.width <= header.origin.x + header.size.width);
    assert!(table.origin.x >= content.origin.x);
    assert!(table.origin.x + table.size.width <= content.origin.x + content.size.width);
}

/// 用高核心数快照验证关键 CPU 状态和核心网格在窄、常规、宽窗口中都不越界。
#[gpui::test]
#[allow(clippy::expect_used)]
fn performance_layout_keeps_cpu_state_inside_parent_at_supported_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let snapshot = MonitorSnapshot {
        cpu_percent: 42.0,
        core_usages: vec![42.0; 128],
        core_histories: vec![vec![[0.0, 42.0]]; 128],
        ..MonitorSnapshot::default()
    };
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let monitor = SystemMonitor::new();
        monitor.set_snapshot_for_test(snapshot);
        let view = cx.new(|_| SystemView {
            monitor,
            section: SystemSection::Performance,
            termination_request: None,
            termination_in_progress: false,
            notice: None,
        });
        Root::new(view, window, cx)
    });

    for (width, height) in [(360.0, 640.0), (1024.0, 720.0), (1440.0, 900.0)] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.run_until_parked();

        let body = cx
            .debug_bounds("system-performance-body")
            .expect("performance body should be rendered");
        let metric = cx
            .debug_bounds("system-metric-card-CPU")
            .expect("CPU metric card should be rendered");
        let value = cx
            .debug_bounds("system-metric-value-CPU")
            .expect("CPU metric value should be rendered");
        let detail = cx
            .debug_bounds("system-metric-detail-CPU")
            .expect("CPU core count should be rendered");
        let cores = cx
            .debug_bounds("system-core-panel")
            .expect("core panel should be rendered");
        let grid = cx
            .debug_bounds("system-core-grid")
            .expect("core grid should be rendered");
        let last_tile = cx
            .debug_bounds("system-core-tile-128")
            .expect("last core tile should be rendered");

        assert!(metric.size.width > px(0.0));
        assert!(cores.size.width > px(0.0));
        assert_inside(&body, &metric, "CPU metric card");
        assert_inside(&body, &cores, "CPU core panel");
        assert_inside(&metric, &value, "CPU metric value");
        assert_inside(&metric, &detail, "CPU core count");
        assert_inside(&cores, &grid, "CPU core grid");
        assert_inside(&cores, &last_tile, "last CPU core tile");
        assert_inside(&grid, &last_tile, "last CPU core tile in grid");
    }
}
