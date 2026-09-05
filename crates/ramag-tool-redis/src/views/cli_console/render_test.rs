#![allow(clippy::expect_used)]

use super::{CliConsole, Entry, Outcome};
use crate::views::key_detail::render_test::{mock_config, mock_service};
use gpui::{AppContext as _, Bounds, Pixels, TestAppContext, px, size};

fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
    assert!(
        child.origin.x >= parent.origin.x
            && child.origin.y >= parent.origin.y
            && child.right() <= parent.right()
            && child.bottom() <= parent.bottom(),
        "{label} 越出父容器：parent={parent:?}, child={child:?}"
    );
}

/// 命令历史和生产保护文案变长时，固定操作仍应留在控制台工具栏内。
#[gpui::test]
fn cli_toolbar_wraps_status_and_keeps_clear_inside_supported_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mut config = mock_config();
    config.production = true;
    let mut console_entity = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let console = cx.new(|cx| CliConsole::new(mock_service(), config.clone(), 0, window, cx));
        console_entity = Some(console.clone());
        gpui_component::Root::new(console, window, cx)
    });
    let console = console_entity.expect("Redis 命令控制台应初始化");
    console.update(cx, |console, cx| {
        console.history = (0..8)
            .map(|id| Entry {
                id,
                command: "BLPOP production_queue 30".into(),
                db: 0,
                outcome: Outcome::Pending,
                display_lines: 1,
                elapsed_ms: 0,
                raw: None,
                cursor: None,
            })
            .collect();
        console.rebuild_transcript_rows();
        cx.notify();
    });

    for width in [180.0, 280.0, 600.0] {
        cx.simulate_resize(size(px(width), px(360.0)));
        cx.run_until_parked();

        let toolbar = cx
            .debug_bounds("redis-cli-toolbar")
            .expect("Redis 命令工具栏应渲染");
        let history = cx
            .debug_bounds("redis-cli-history")
            .expect("命令历史状态应渲染");
        let read_only = cx
            .debug_bounds("redis-cli-read-only")
            .expect("生产只读状态应渲染");
        let clear = cx.debug_bounds("redis-cli-clear").expect("清空按钮应渲染");
        assert_inside(&toolbar, &history, "命令历史状态");
        assert_inside(&toolbar, &read_only, "生产只读状态");
        assert_inside(&toolbar, &clear, "清空按钮");
        assert!(clear.size.width > px(0.0));
        assert!(clear.size.height > px(0.0));
    }
}
