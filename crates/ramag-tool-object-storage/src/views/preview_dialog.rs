//! 对象文本内容的独立居中查看窗口。

use gpui::{
    ClickEvent, Context, Entity, InteractiveElement as _, ParentElement as _, Styled as _, Window,
    div, px,
};
use gpui_component::{
    IconName, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex, input::Input,
    input::InputState, v_flex,
};

use super::model::ObjectStorageView;

pub(super) fn open_object_preview_dialog(
    key: String,
    summary: String,
    editor: Entity<InputState>,
    line_count: usize,
    window: &mut Window,
    cx: &mut Context<ObjectStorageView>,
) {
    let title = key
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(&key)
        .to_string();
    window.open_dialog(cx, move |dialog, window, _| {
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) * 0.72)
            .clamp(640.0, 1_120.0)
            .min((f32::from(viewport.width) - 32.0).max(0.0));
        // 与 SFTP 文件查看器保持相同的宽度、行高和垂直居中规则。
        let max_editor_height = (f32::from(viewport.height) - 112.0 - 48.0).max(100.0);
        let editor_height =
            ((line_count.saturating_add(3).max(3) as f32 * 22.0) + 12.0).min(max_editor_height);
        let margin_top = ((f32::from(viewport.height) - editor_height - 112.0) / 2.0).max(24.0);
        let content_editor = editor.clone();
        let summary = summary.clone();
        dialog
            .title(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(title.clone()),
                    )
                    .child(
                        ramag_ui::clickable_button("object-preview-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("关闭")
                            .on_click(move |_: &ClickEvent, window, app| {
                                window.close_dialog(app);
                            }),
                    ),
            )
            .close_button(false)
            .overlay_closable(true)
            .keyboard(false)
            .margin_top(px(margin_top))
            .w(px(width))
            .p(px(14.0))
            .content(move |content, _, _| {
                content.child(
                    v_flex()
                        .w_full()
                        .gap(px(8.0))
                        .child(div().text_xs().child(summary.clone()))
                        .child(
                            div()
                                .id("object-preview-content")
                                .debug_selector(|| "object-preview-content".into())
                                .w_full()
                                .h(px(editor_height))
                                .overflow_hidden()
                                .child(
                                    Input::new(&content_editor)
                                        .h_full()
                                        .opacity(1.0)
                                        .disabled(true),
                                ),
                        ),
                )
            })
    });
}
