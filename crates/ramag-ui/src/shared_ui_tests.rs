#![allow(clippy::expect_used)]

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px, size,
};

use super::{clickable_button, closable_dialog_title, dialog_action_footer};

struct DialogTitleHost;

struct DialogFooterHost;

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

impl Render for DialogFooterHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Sizable as _, button::ButtonVariants as _};

        div()
            .id("shared-dialog-footer-host")
            .debug_selector(|| "shared-dialog-footer-host".into())
            .w(px(180.0))
            .h(px(120.0))
            .child(dialog_action_footer(
                clickable_button("shared-dialog-footer-secondary")
                    .debug_selector(|| "shared-dialog-footer-secondary".into())
                    .small()
                    .ghost()
                    .label("取消当前同步任务并返回"),
                clickable_button("shared-dialog-footer-primary")
                    .debug_selector(|| "shared-dialog-footer-primary".into())
                    .small()
                    .primary()
                    .label("确认继续执行危险操作"),
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

#[gpui::test]
fn dialog_action_footer_wraps_long_actions_inside_parent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| DialogFooterHost);
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(240.0), px(180.0)));
    cx.run_until_parked();

    let host = cx
        .debug_bounds("shared-dialog-footer-host")
        .expect("操作区宿主应渲染");
    let footer = cx
        .debug_bounds("ramag-dialog-footer")
        .expect("共享操作区应渲染");
    let secondary = cx
        .debug_bounds("shared-dialog-footer-secondary")
        .expect("次要操作按钮应渲染");
    let primary = cx
        .debug_bounds("shared-dialog-footer-primary")
        .expect("主要操作按钮应渲染");

    for button in [secondary, primary] {
        assert!(button.origin.x >= footer.origin.x);
        assert!(button.origin.y >= footer.origin.y);
        assert!(button.origin.x + button.size.width <= footer.origin.x + footer.size.width);
        assert!(button.origin.y + button.size.height <= footer.origin.y + footer.size.height);
        assert!(button.origin.x >= host.origin.x);
        assert!(button.origin.y >= host.origin.y);
        assert!(button.origin.x + button.size.width <= host.origin.x + host.size.width);
        assert!(button.origin.y + button.size.height <= host.origin.y + host.size.height);
    }
    assert!(primary.origin.y > secondary.origin.y);
}
