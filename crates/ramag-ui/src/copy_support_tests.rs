use super::{SelectableText, copy_success_notification};
use gpui::{
    AppContext as _, Context, Element as _, InteractiveElement as _, IntoElement, Modifiers,
    MouseButton, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext,
    Window, div, point, px, size,
};
use gpui_component::WindowExt as _;
use ramag_domain::entities::TransferSummary;

struct SelectableTextHost;

struct NotificationHost;

impl Render for SelectableTextHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "copy-support-test".into())
            .w(px(900.0))
            .h(px(100.0))
            .child(
                SelectableText::new(
                    "copy-support-test",
                    "**bold** [link](https://example.com) `code`\nraw ~ text",
                )
                .w_full()
                .h_full(),
            )
    }
}

impl Render for NotificationHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "copy-notification-host".into())
            .relative()
            .size_full()
            .children(gpui_component::Root::render_notification_layer(window, cx))
    }
}

#[gpui::test]
fn selectable_text_copies_dragged_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| SelectableTextHost);
        gpui_component::Root::new(host, window, cx)
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("copy-support-test")
            .is_some_and(|bounds| bounds.contains(&point(px(10.0), px(20.0))))
    );
    cx.simulate_mouse_down(
        point(px(0.0), px(20.0)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(px(900.0), px(90.0)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(px(900.0), px(90.0)),
        MouseButton::Left,
        Modifiers::default(),
    );
    #[cfg(target_os = "macos")]
    cx.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    cx.simulate_keystrokes("ctrl-c");

    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(
        copied,
        "**bold** [link](https://example.com) `code`\nraw ~ text"
    );
}

#[gpui::test]
fn copy_notification_stays_inside_narrow_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| NotificationHost);
        gpui_component::Root::new(host, window, cx)
    });
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(180.0), px(120.0)));
    let notification = copy_success_notification().content(|_, _, _| {
        div()
            .debug_selector(|| "copy-success-notification-content".into())
            .size(px(1.0))
            .into_any()
    });
    cx.update(|window, cx| window.push_notification(notification, cx));
    cx.run_until_parked();

    let notification = cx.debug_bounds("copy-success-notification-content");
    assert!(notification.is_some(), "复制成功通知应渲染");
    if let Some(notification) = notification {
        assert!(notification.origin.x >= px(0.0));
        assert!(notification.origin.y >= px(0.0));
        assert!(notification.right() <= px(180.0));
        assert!(notification.bottom() <= px(120.0));
    }
}

#[gpui::test]
fn transfer_notification_stays_inside_narrow_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| NotificationHost);
        gpui_component::Root::new(host, window, cx)
    });
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(180.0), px(220.0)));

    let notification = crate::transfer_notification(
        "导出",
        "任务已取消",
        Ok(Some((
            TransferSummary {
                objects: 12,
                items: 3_600,
                failed: 2,
                cancelled: true,
                ..TransferSummary::default()
            },
            "连接名称很长，仍应保留在通知范围内".to_string(),
        ))),
    );
    assert!(notification.is_some(), "传输结果应生成通知");
    if let Some(notification) = notification {
        let notification = notification.content(|_, _, _| {
            div()
                .debug_selector(|| "transfer-notification-content".into())
                .w_full()
                .min_w_0()
                .child("传输结果")
                .into_any()
        });
        cx.update(|window, cx| window.push_notification(notification, cx));
        cx.run_until_parked();

        let notification = cx.debug_bounds("transfer-notification-content");
        assert!(notification.is_some(), "传输通知应渲染");
        if let Some(notification) = notification {
            assert!(notification.origin.x >= px(0.0));
            assert!(notification.origin.y >= px(0.0));
            assert!(notification.right() <= px(180.0));
            assert!(notification.bottom() <= px(220.0));
        }
    }
}
