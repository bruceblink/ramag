#![allow(clippy::expect_used, clippy::type_complexity)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, MouseButton, ParentElement, Render,
    Styled, TestAppContext, VisualTestContext, Window, div, point, prelude::*, px, size,
};
use ramag_app::{DataSyncExecutionContext, DataSyncGate, DataSyncGatePhase};
use ramag_domain::entities::{DataSyncSummary, DataSyncTaskId};

use super::{DataSyncOverlay, format_count, format_elapsed_ms};

struct OverlayTestHost {
    overlay: Entity<DataSyncOverlay>,
    background_mouse_events: Arc<AtomicUsize>,
    background_key_events: Arc<AtomicUsize>,
    render_count: Arc<AtomicUsize>,
}

impl Render for OverlayTestHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.render_count.fetch_add(1, Ordering::Relaxed);
        let mouse_events = self.background_mouse_events.clone();
        let key_events = self.background_key_events.clone();
        div()
            .id("overlay-test-host")
            .debug_selector(|| "overlay-test-host".into())
            .relative()
            .size_full()
            .on_key_down(move |_, _, _| {
                key_events.fetch_add(1, Ordering::Relaxed);
            })
            .child(
                div()
                    .id("overlay-behind-control")
                    .debug_selector(|| "overlay-behind-control".into())
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        mouse_events.fetch_add(1, Ordering::Relaxed);
                    }),
            )
            .child(self.overlay.clone())
    }
}

fn context() -> DataSyncExecutionContext {
    DataSyncExecutionContext {
        source_connection: "source".into(),
        source_scope: "source-db".into(),
        target_connection: "target".into(),
        target_scope: "target-db".into(),
    }
}

fn add_overlay_window(
    cx: &mut TestAppContext,
    gate: Arc<DataSyncGate>,
) -> (
    Entity<OverlayTestHost>,
    Entity<DataSyncOverlay>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    &mut VisualTestContext,
) {
    cx.update(gpui_component::init);
    let mouse_events = Arc::new(AtomicUsize::new(0));
    let key_events = Arc::new(AtomicUsize::new(0));
    let render_count = Arc::new(AtomicUsize::new(0));
    let mut overlay_entity = None;
    let mouse_for_view = mouse_events.clone();
    let keys_for_view = key_events.clone();
    let renders_for_view = render_count.clone();
    let (host, visual_cx) = cx.add_window_view(|_window, cx| {
        let overlay = cx.new(|cx| DataSyncOverlay::new(gate, cx));
        overlay_entity = Some(overlay.clone());
        OverlayTestHost {
            overlay,
            background_mouse_events: mouse_for_view,
            background_key_events: keys_for_view,
            render_count: renders_for_view,
        }
    });
    (
        host,
        overlay_entity.expect("占屏实体应创建"),
        mouse_events,
        key_events,
        render_count,
        visual_cx,
    )
}

#[gpui::test]
fn running_overlay_covers_window_and_blocks_background_input(cx: &mut TestAppContext) {
    let gate = Arc::new(DataSyncGate::default());
    let _permit = gate
        .begin(DataSyncTaskId::new(), context())
        .expect("门禁应开始");
    let (host_entity, _overlay, mouse_events, key_events, render_count, cx) =
        add_overlay_window(cx, gate);
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    host_entity.update(cx, |_, cx| cx.notify());
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(render_count.load(Ordering::Relaxed) > 0, "宿主必须发生渲染");

    let host = cx.debug_bounds("overlay-test-host").expect("宿主应渲染");
    let overlay = cx.debug_bounds("data-sync-overlay").expect("占屏应渲染");
    assert_eq!(overlay, host, "占屏必须覆盖完整主视图");
    assert!(cx.debug_bounds("data-sync-card").is_some());

    let behind = cx
        .debug_bounds("overlay-behind-control")
        .expect("背后控件应参与布局");
    let click = point(behind.origin.x + px(8.0), behind.origin.y + px(8.0));
    cx.simulate_mouse_down(click, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(click, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(click, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(click, MouseButton::Right, Modifiers::default());
    cx.simulate_keystrokes("tab enter escape");
    assert_eq!(mouse_events.load(Ordering::Relaxed), 0);
    assert_eq!(key_events.load(Ordering::Relaxed), 0);
}

#[gpui::test]
fn cancel_and_terminal_result_keep_overlay_until_acknowledged(cx: &mut TestAppContext) {
    let gate = Arc::new(DataSyncGate::default());
    let permit = gate
        .begin(DataSyncTaskId::new(), context())
        .expect("门禁应开始");
    let (host_entity, overlay_entity, _mouse_events, _key_events, _render_count, cx) =
        add_overlay_window(cx, gate.clone());
    host_entity.update(cx, |_, cx| cx.notify());
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let cancel = cx.debug_bounds("sync-cancel").expect("取消按钮应渲染");
    let cancel_point = point(
        cancel.origin.x + cancel.size.width / 2.0,
        cancel.origin.y + cancel.size.height / 2.0,
    );
    cx.simulate_mouse_down(cancel_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(cancel_point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let confirm = cx
        .debug_bounds("sync-cancel-confirm")
        .expect("二次取消确认应渲染");
    let confirm_point = point(
        confirm.origin.x + confirm.size.width / 2.0,
        confirm.origin.y + confirm.size.height / 2.0,
    );
    cx.simulate_mouse_down(confirm_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(confirm_point, MouseButton::Left, Modifiers::default());
    assert_eq!(
        gate.snapshot().map(|snapshot| snapshot.phase),
        Some(DataSyncGatePhase::Cancelling)
    );
    assert!(gate.is_blocking());

    assert!(gate.finish_cancelled(&permit, DataSyncSummary::default()));
    overlay_entity.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds("data-sync-overlay").is_some());
    assert!(cx.debug_bounds("sync-result-summary").is_some());
    assert!(cx.debug_bounds("sync-running-progress").is_none());
    let acknowledge = cx
        .debug_bounds("sync-result-ack")
        .expect("终态确认按钮应渲染");
    let acknowledge_point = point(
        acknowledge.origin.x + acknowledge.size.width / 2.0,
        acknowledge.origin.y + acknowledge.size.height / 2.0,
    );
    cx.simulate_mouse_down(acknowledge_point, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(acknowledge_point, MouseButton::Left, Modifiers::default());
    assert!(!gate.is_blocking());
}

#[gpui::test]
fn failed_result_remains_readable_inside_small_window(cx: &mut TestAppContext) {
    let gate = Arc::new(DataSyncGate::default());
    let permit = gate
        .begin(DataSyncTaskId::new(), context())
        .expect("门禁应开始");
    assert!(gate.finish_failed(
        &permit,
        DataSyncSummary::default(),
        "包含中文的长错误信息".repeat(2_000),
    ));
    let (host_entity, _overlay, _mouse_events, _key_events, _render_count, cx) =
        add_overlay_window(cx, gate);
    cx.simulate_resize(size(px(320.0), px(240.0)));
    host_entity.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let overlay = cx.debug_bounds("data-sync-overlay").expect("占屏应渲染");
    let card = cx.debug_bounds("data-sync-card").expect("结果卡片应渲染");
    assert!(card.size.width <= overlay.size.width);
    assert!(card.size.height <= overlay.size.height);
    assert!(cx.debug_bounds("sync-result-ack").is_some());
}

#[test]
fn result_numbers_and_elapsed_time_are_human_readable_at_boundaries() {
    assert_eq!(format_count(0), "0");
    assert_eq!(format_count(999), "999");
    assert_eq!(format_count(1_000), "1,000");
    assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");

    assert_eq!(format_elapsed_ms(0), "0.00 秒");
    assert_eq!(format_elapsed_ms(59_999), "59.99 秒");
    assert_eq!(format_elapsed_ms(60_000), "1 分 00.00 秒");
    assert_eq!(format_elapsed_ms(94_120), "1 分 34.12 秒");
    assert_eq!(format_elapsed_ms(3_661_230), "1 小时 1 分 01.23 秒");
}
