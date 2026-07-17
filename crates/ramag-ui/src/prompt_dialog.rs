//! 单行文本输入对话框（与 open_confirm 对称）。用于重命名等需要输入新值的轻量操作；
//! 确认时把 trim 后的输入交给 on_confirm，空输入不触发

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, ClickEvent, Entity, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    v_flex,
};

const MAX_PROMPT_INPUT_BYTES: usize = 1024 * 1024;

pub fn open_prompt(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    initial: &str,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    open_prompt_impl(
        title.into(),
        description.into(),
        initial,
        confirm_label.into(),
        MAX_PROMPT_INPUT_BYTES,
        false,
        on_confirm,
        window,
        cx,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn open_bounded_prompt(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    initial: &str,
    confirm_label: impl Into<SharedString>,
    max_bytes: usize,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    open_prompt_impl(
        title.into(),
        description.into(),
        initial,
        confirm_label.into(),
        max_bytes,
        false,
        on_confirm,
        window,
        cx,
    );
}

/// 允许提交空字符串，用于“留空即采用后端默认值”的场景。
pub fn open_optional_prompt(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    initial: &str,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    open_prompt_impl(
        title.into(),
        description.into(),
        initial,
        confirm_label.into(),
        MAX_PROMPT_INPUT_BYTES,
        true,
        on_confirm,
        window,
        cx,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn open_optional_bounded_prompt(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    initial: &str,
    confirm_label: impl Into<SharedString>,
    max_bytes: usize,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    open_prompt_impl(
        title.into(),
        description.into(),
        initial,
        confirm_label.into(),
        max_bytes,
        true,
        on_confirm,
        window,
        cx,
    );
}

#[allow(clippy::too_many_arguments)]
fn open_prompt_impl(
    title: SharedString,
    description: SharedString,
    initial: &str,
    confirm_label: SharedString,
    max_bytes: usize,
    allow_empty: bool,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if max_bytes == 0 || initial.len() > max_bytes {
        window.push_notification(
            Notification::error(format!(
                "输入内容超过 {max_bytes} bytes 上限，无法打开编辑对话框"
            )),
            cx,
        );
        return;
    }
    let input: Entity<InputState> = cx.new(|cx| {
        InputState::new(window, cx)
            .validate(move |value, _| value.len() <= max_bytes)
            .default_value(initial)
    });
    // 打开即聚焦输入框：重命名类操作无需先点一下即可编辑
    input.update(cx, |state, cx| {
        state.focus(window, cx);
    });
    // FnOnce 包成可 Clone 的 Fn 句柄
    let on_confirm_cell = Rc::new(RefCell::new(Some(on_confirm)));

    window.open_dialog(cx, move |dialog, _, _| {
        let desc = description.clone();
        let confirm_label_inner = confirm_label.clone();

        let cancel_btn = Button::new("ramag-prompt-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| {
                window.close_dialog(app);
            });

        let ok_btn = Button::new("ramag-prompt-ok")
            .small()
            .primary()
            .label(confirm_label_inner)
            .on_click({
                let cell = on_confirm_cell.clone();
                let input = input.clone();
                move |_: &ClickEvent, window, app| {
                    let Some(value) =
                        normalize_prompt_value(input.read(app).value().as_ref(), allow_empty)
                    else {
                        return;
                    };
                    if let Some(cb) = cell.borrow_mut().take() {
                        cb(value, window, app);
                    }
                    window.close_dialog(app);
                }
            });

        let input_for_content = input.clone();
        dialog
            .title(title.clone())
            .margin_top(px(180.0))
            // 键盘 Enter：与 ok 按钮同逻辑（读输入、空则不关、非空执行）。
            // 返回 false 时对话框不关闭——空输入下回车保持打开，等用户填内容
            .on_ok({
                let cell = on_confirm_cell.clone();
                let input = input.clone();
                move |_, window, app| {
                    let Some(value) =
                        normalize_prompt_value(input.read(app).value().as_ref(), allow_empty)
                    else {
                        return false;
                    };
                    if let Some(cb) = cell.borrow_mut().take() {
                        cb(value, window, app);
                    }
                    true
                }
            })
            .content(move |content, _, cx| {
                let muted_fg = cx.theme().muted_foreground;
                content.child(
                    v_flex()
                        .gap(px(8.0))
                        .py(px(4.0))
                        .child(div().text_sm().text_color(muted_fg).child(desc.clone()))
                        .child(Input::new(&input_for_content).small()),
                )
            })
            .footer(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(cancel_btn)
                    .child(ok_btn),
            )
    });
}

fn normalize_prompt_value(value: &str, allow_empty: bool) -> Option<String> {
    let value = value.trim();
    if value.is_empty() && !allow_empty {
        return None;
    }
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_prompt_value;

    #[test]
    fn required_and_optional_prompts_handle_empty_values_differently() {
        assert_eq!(
            normalize_prompt_value("  name  ", false).as_deref(),
            Some("name")
        );
        assert_eq!(normalize_prompt_value("   ", false), None);
        assert_eq!(normalize_prompt_value("   ", true).as_deref(), Some(""));
    }
}
