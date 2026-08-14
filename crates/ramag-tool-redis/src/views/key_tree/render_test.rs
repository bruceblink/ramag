//! Redis Key 树交互测试：复制不能误触选择，子节点必须呈现明确层级引导。
#![allow(clippy::expect_used)]

use gpui::{
    AppContext as _, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Point, TestAppContext,
    VisualTestContext, px, size,
};
use ramag_domain::entities::KeyMeta;

use super::{INDENT_PX, KeyTreePanel};
use crate::views::key_detail::render_test::{mock_config, mock_service};

fn simulate_click_count(
    cx: &mut VisualTestContext,
    position: Point<gpui::Pixels>,
    modifiers: Modifiers,
    click_count: usize,
) {
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count,
        first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count,
    });
}

#[gpui::test]
fn modifier_double_click_copies_full_key_without_selecting(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mut panel_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| {
            let mut panel = KeyTreePanel::new(mock_service(), window, cx);
            panel.config = Some(mock_config());
            panel.keys = vec![KeyMeta::bare("17xxx27:code")];
            panel.rebuild_tree();
            panel.expanded.insert("17xxx27".into());
            panel.expanded_revision = panel.expanded_revision.wrapping_add(1);
            panel
        });
        panel_entity = Some(panel.clone());
        gpui_component::Root::new(panel, window, cx)
    });
    let panel = panel_entity.expect("KeyTreePanel should be initialized");
    cx.simulate_resize(size(px(420.0), px(480.0)));
    cx.run_until_parked();

    let child_row = cx
        .debug_bounds("redis-tree-row-1")
        .expect("子 Key 行应参与布局");
    let guides = cx
        .debug_bounds("redis-tree-guides-1")
        .expect("子 Key 行应包含层级引导");
    assert_eq!(guides.size.width, px(INDENT_PX));
    assert!(guides.size.height > px(0.0));

    let modifiers = Modifiers::secondary_key();
    let position = child_row.center();
    simulate_click_count(cx, position, modifiers, 1);
    assert!(panel.read_with(cx, |panel, _| panel.selected.is_none()));
    simulate_click_count(cx, position, modifiers, 2);

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("17xxx27:code".into())
    );
    assert!(panel.read_with(cx, |panel, _| panel.selected.is_none()));

    cx.simulate_click(position, Modifiers::default());
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected.clone()),
        Some("17xxx27:code".into())
    );

    let namespace_row = cx
        .debug_bounds("redis-tree-row-0")
        .expect("命名空间行应参与布局");
    simulate_click_count(cx, namespace_row.center(), modifiers, 1);
    simulate_click_count(cx, namespace_row.center(), modifiers, 2);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("17xxx27".into())
    );
    assert!(panel.read_with(cx, |panel, _| panel.expanded.contains("17xxx27")));
}
