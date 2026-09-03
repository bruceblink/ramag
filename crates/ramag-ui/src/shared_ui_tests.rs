#![allow(clippy::expect_used)]

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px, size,
};

use super::closable_dialog_title;

struct DialogTitleHost;

impl Render for DialogTitleHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("shared-dialog-title-host")
            .debug_selector(|| "shared-dialog-title-host".into())
            .size_full()
            .child(closable_dialog_title(
                "shared-dialog-title-close",
                "这是一个用于验证窄窗口标题收缩边界的长对话框标题",
                |_, _| {},
            ))
    }
}

#[gpui::test]
fn closable_dialog_title_keeps_close_button_inside_narrow_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| DialogTitleHost);
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(240.0), px(120.0)));
    cx.run_until_parked();

    let host = cx
        .debug_bounds("shared-dialog-title-host")
        .expect("标题宿主应渲染");
    let title = cx
        .debug_bounds("ramag-dialog-title")
        .expect("共享标题应渲染");
    let close = cx
        .debug_bounds("ramag-dialog-close")
        .expect("关闭按钮应渲染");

    assert!(title.origin.x >= host.origin.x);
    assert!(title.origin.x + title.size.width <= host.origin.x + host.size.width);
    assert!(close.origin.x >= title.origin.x);
    assert!(close.origin.x + close.size.width <= title.origin.x + title.size.width);
}
