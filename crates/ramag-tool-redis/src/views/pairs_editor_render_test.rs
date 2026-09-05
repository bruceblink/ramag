#![allow(clippy::expect_used)]

use super::{PairsEditor, PairsKind};
use gpui::{AppContext as _, Bounds, Pixels, TestAppContext, px, size};
use gpui_component::Root;

fn assert_inside(parent: &Bounds<Pixels>, child: &Bounds<Pixels>, label: &str) {
    assert!(
        child.origin.x >= parent.origin.x
            && child.origin.y >= parent.origin.y
            && child.right() <= parent.right()
            && child.bottom() <= parent.bottom(),
        "{label} 越出父容器：parent={parent:?}, child={child:?}"
    );
}

/// 双列编辑器在窄窗口中把字段和值分行，同时保留添加和删除操作。
#[gpui::test]
fn pairs_editor_wraps_fields_and_actions_inside_supported_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let editor = cx.new(|cx| {
            let mut editor = PairsEditor::new(PairsKind::Hash, window, cx);
            editor.add_row(window, cx);
            editor
        });
        Root::new(editor, window, cx)
    });

    for width in [180.0, 280.0, 600.0] {
        cx.simulate_resize(size(px(width), px(420.0)));
        cx.run_until_parked();

        let editor = cx
            .debug_bounds("redis-pairs-editor")
            .expect("Redis Hash 编辑器应渲染");
        let toolbar = cx
            .debug_bounds("redis-pairs-toolbar")
            .expect("Redis Hash 工具栏应渲染");
        let add = cx.debug_bounds("redis-pairs-add").expect("添加按钮应渲染");
        let count = cx
            .debug_bounds("redis-pairs-count")
            .expect("字段数量状态应渲染");
        let first_row = cx.debug_bounds("redis-pairs-row-0").expect("第一行应渲染");
        let second_row = cx.debug_bounds("redis-pairs-row-1").expect("第二行应渲染");
        let left = cx
            .debug_bounds("redis-pairs-left-0")
            .expect("字段输入框应渲染");
        let right = cx
            .debug_bounds("redis-pairs-right-0")
            .expect("值输入框应渲染");
        let remove = cx
            .debug_bounds("redis-pairs-remove-1")
            .expect("删除按钮应渲染");

        assert_inside(&editor, &toolbar, "双列工具栏");
        assert_inside(&editor, &first_row, "第一行");
        assert_inside(&editor, &second_row, "第二行");
        assert_inside(&toolbar, &add, "添加按钮");
        assert_inside(&toolbar, &count, "字段数量状态");
        assert_inside(&first_row, &left, "字段输入框");
        assert_inside(&first_row, &right, "值输入框");
        assert_inside(&second_row, &remove, "删除按钮");
        for (control, name) in [
            (add, "添加按钮"),
            (count, "字段数量状态"),
            (left, "字段输入框"),
            (right, "值输入框"),
            (remove, "删除按钮"),
        ] {
            assert!(control.size.width > px(0.0), "{name} 不能为零宽");
            assert!(control.size.height > px(0.0), "{name} 不能为零高");
        }

        if width == 180.0 {
            assert!(
                right.origin.y > left.origin.y,
                "最小宽度应让字段和值分行：left={left:?}, right={right:?}"
            );
        }
    }
}
