#![allow(clippy::expect_used)]

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, TestAppContext, VisualTestContext, Window, div, px, size,
};
use gpui_component::input::InputState;

use super::{
    centered_status, clickable_button, closable_dialog_title, dialog_action_footer,
    responsive_toolbar,
};

struct DialogTitleHost;

struct DialogFooterHost;

struct CleanableInputHost {
    input: Entity<InputState>,
}

struct CenteredStatusHost;

struct ResponsiveToolbarHost;

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

impl Render for CleanableInputHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("shared-cleanable-input-host")
            .debug_selector(|| "shared-cleanable-input-host".into())
            .w(px(128.0))
            .h(px(48.0))
            .child(super::cleanable_input(
                &self.input,
                "shared-cleanable-input-clear",
                false,
                cx,
            ))
    }
}

impl Render for CenteredStatusHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::ActiveTheme as _;

        div()
            .id("shared-centered-status-host")
            .debug_selector(|| "shared-centered-status-host".into())
            .w(px(180.0))
            .h(px(100.0))
            .child(centered_status(
                "没有匹配当前搜索条件的历史记录，请缩短条件后重试",
                cx.theme().muted_foreground,
            ))
    }
}

impl Render for ResponsiveToolbarHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("shared-responsive-toolbar-host")
            .debug_selector(|| "shared-responsive-toolbar-host".into())
            .w(px(180.0))
            .h(px(80.0))
            .child(
                responsive_toolbar()
                    .debug_selector(|| "ramag-responsive-toolbar".into())
                    .child(
                        div()
                            .debug_selector(|| "shared-responsive-toolbar-summary".into())
                            .w(px(140.0))
                            .h(px(20.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .debug_selector(|| "shared-responsive-toolbar-action".into())
                            .w(px(60.0))
                            .h(px(20.0))
                            .flex_none(),
                    ),
            )
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

#[gpui::test]
fn cleanable_input_keeps_clear_button_inside_narrow_parent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| {
            state.set_value("一段足够长的搜索内容", window, cx);
        });
        CleanableInputHost { input }
    });
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(240.0), px(120.0)));
    cx.run_until_parked();

    let host = cx
        .debug_bounds("shared-cleanable-input-host")
        .expect("清除输入框宿主应渲染");
    let clear = cx
        .debug_bounds("shared-cleanable-input-clear")
        .expect("非空输入应显示清除按钮");

    assert!(clear.origin.x >= host.origin.x);
    assert!(clear.origin.y >= host.origin.y);
    assert!(clear.origin.x + clear.size.width <= host.origin.x + host.size.width);
    assert!(clear.origin.y + clear.size.height <= host.origin.y + host.size.height);
}

#[gpui::test]
fn centered_status_keeps_long_message_inside_narrow_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| CenteredStatusHost);
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(180.0), px(120.0)));
    cx.run_until_parked();

    let host = cx
        .debug_bounds("shared-centered-status-host")
        .expect("状态提示宿主应渲染");
    let status = cx
        .debug_bounds("ramag-centered-status-message")
        .expect("状态提示文本区域应渲染");

    assert!(status.origin.x >= host.origin.x);
    assert!(status.origin.y >= host.origin.y);
    assert!(status.origin.x + status.size.width <= host.origin.x + host.size.width);
    assert!(status.origin.y + status.size.height <= host.origin.y + host.size.height);
}

#[gpui::test]
fn responsive_toolbar_wraps_fixed_actions_inside_narrow_parent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| ResponsiveToolbarHost);
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(240.0), px(120.0)));
    cx.run_until_parked();

    let host = cx
        .debug_bounds("shared-responsive-toolbar-host")
        .expect("工具栏宿主应渲染");
    let toolbar = cx
        .debug_bounds("ramag-responsive-toolbar")
        .expect("共享工具栏应渲染");
    let summary = cx
        .debug_bounds("shared-responsive-toolbar-summary")
        .expect("工具栏摘要应渲染");
    let action = cx
        .debug_bounds("shared-responsive-toolbar-action")
        .expect("工具栏操作项应渲染");

    for child in [summary, action] {
        assert!(child.origin.x >= toolbar.origin.x);
        assert!(child.origin.y >= toolbar.origin.y);
        assert!(child.right() <= toolbar.right());
        assert!(child.bottom() <= toolbar.bottom());
        assert!(child.origin.x >= host.origin.x);
        assert!(child.origin.y >= host.origin.y);
        assert!(child.right() <= host.right());
        assert!(child.bottom() <= host.bottom());
    }
    assert!(action.origin.y > summary.origin.y);
}
