use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div, point,
    px,
};

use super::SelectableText;

struct SelectableTextHost;

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

#[gpui::test]
fn selectable_text_copies_dragged_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| SelectableTextHost);
        gpui_component::Root::new(host, window, cx)
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("copy-support-test")
        .expect("selectable text should be rendered");
    assert!(bounds.contains(&point(px(10.0), px(20.0))));
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
