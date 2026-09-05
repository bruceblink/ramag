#![allow(clippy::expect_used)]

use super::{add_vcs_window, inject_diff_session};
use gpui::{Bounds, Pixels, TestAppContext, px, size};

fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
    assert!(
        child.origin.x >= parent.origin.x
            && child.origin.y >= parent.origin.y
            && child.right() <= parent.right()
            && child.bottom() <= parent.bottom(),
        "{label} 越出父容器：parent={parent:?}, child={child:?}"
    );
}

/// 文件栏在最小可拖动宽度下，模式、搜索和固定操作都不能越出工具栏。
#[gpui::test]
fn vcs_files_toolbar_wraps_controls_inside_supported_widths(cx: &mut TestAppContext) {
    let (view, cx) = add_vcs_window(cx);
    view.update(cx, |view, cx| {
        inject_diff_session(view);
        cx.notify();
    });
    cx.simulate_resize(size(px(1440.0), px(720.0)));
    cx.run_until_parked();

    for width in [180.0, 280.0, 600.0] {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.ide_left_resize.update(cx, |state, cx| {
                    state.resize_panel(0, px(width), window, cx);
                });
            });
        });
        cx.run_until_parked();

        let files_column = cx
            .debug_bounds("vcs-files-column")
            .expect("VCS 文件栏应渲染");
        let toolbar = cx
            .debug_bounds("vcs-files-toolbar")
            .expect("VCS 文件栏工具栏应渲染");
        let mode_toolbar = cx
            .debug_bounds("vcs-files-mode-toolbar")
            .expect("VCS 模式工具栏应渲染");
        let search_toolbar = cx
            .debug_bounds("vcs-files-search-toolbar")
            .expect("VCS 搜索工具栏应渲染");
        let search = cx
            .debug_bounds("vcs-files-search")
            .expect("VCS 文件搜索框应渲染");
        assert_inside(&files_column, &toolbar, "VCS 文件栏工具栏");
        assert_inside(&toolbar, &mode_toolbar, "VCS 模式工具栏");
        assert_inside(&toolbar, &search_toolbar, "VCS 搜索工具栏");
        assert_inside(&search_toolbar, &search, "VCS 文件搜索框");

        for selector in [
            "vcs-files-tab-project",
            "vcs-files-tab-changes",
            "vcs-files-tab-stash",
            "vcs-branch-picker",
            "vcs-refresh",
            "vcs-pf-toggle-all",
            "vcs-history-pane-toggle",
        ] {
            let control = cx.debug_bounds(selector).expect("VCS 文件栏控件应渲染");
            let parent = if selector.starts_with("vcs-files-tab") || selector == "vcs-branch-picker"
            {
                mode_toolbar
            } else {
                search_toolbar
            };
            assert_inside(&parent, &control, selector);
        }

        if width == 180.0 {
            let history = cx
                .debug_bounds("vcs-history-pane-toggle")
                .expect("历史按钮应渲染");
            assert!(
                history.origin.y > search.origin.y,
                "最小文件栏宽度应让部分搜索操作换行：search={search:?}, history={history:?}"
            );
        }
    }
}
