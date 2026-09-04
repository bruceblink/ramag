//! 统一复制交互：主修饰键双击与可拖拽选择的只读文本。

use gpui::{
    App, ClickEvent, ClipboardItem, ElementId, IntoElement, RenderOnce, SharedString,
    StyleRefinement, Styled, Window,
};
use gpui_component::{StyledExt as _, notification::Notification, text::TextView};

/// 判断是否为“主修饰键 + 左键双击”。
///
/// GPUI 的 `Modifiers::secondary` 已按平台映射：macOS 是 Command，Windows/Linux 是 Ctrl。
pub fn is_primary_modifier_double_click(event: &ClickEvent) -> bool {
    event.standard_click() && event.click_count() >= 2 && event.modifiers().secondary()
}

/// 将文本写入系统剪贴板。
pub fn copy_text(text: impl Into<String>, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
}

/// 复制文本并给出统一、简短的成功反馈。
pub fn copy_text_with_notification(text: impl Into<String>, window: &mut Window, cx: &mut App) {
    copy_text(text, cx);
    crate::push_responsive_notification(window, copy_success_notification(), cx);
}

/// 创建紧凑的复制成功通知。
pub fn copy_success_notification() -> Notification {
    crate::responsive_notification(Notification::success("复制成功").autohide(true))
}

/// 可拖拽选中的只读文本。
///
/// GPUI 的普通 `div().child(text)` 只负责绘制，不具备文本选区。这里复用
/// GPUI Component 的 `TextView` 选择实现，并把内容放进 fenced code block，避免
/// 原始文本里的 Markdown 符号被当成富文本语法，从而保证复制的是数据本身。
#[derive(IntoElement)]
pub struct SelectableText {
    id: ElementId,
    text: SharedString,
    style: StyleRefinement,
}

impl SelectableText {
    /// 创建只读可选中文本。
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for SelectableText {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for SelectableText {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        TextView::markdown(self.id, plain_text_markdown(&self.text))
            .selectable(true)
            .refine_style(&self.style)
    }
}

fn plain_text_markdown(text: &str) -> String {
    let mut longest_fence_run: usize = 0;
    let mut current_run: usize = 0;
    for character in text.chars() {
        if character == '`' {
            current_run += 1;
            longest_fence_run = longest_fence_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat(longest_fence_run.saturating_add(1).max(3));
    format!("{fence}\n{text}\n{fence}")
}
