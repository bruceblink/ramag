//! 单行文本输入对话框（与 open_confirm 对称）。用于重命名等需要输入新值的轻量操作；
//! 确认时把 trim 后的输入交给 on_confirm，空输入不触发

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, ClickEvent, Entity, ParentElement, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
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
        false,
        on_confirm,
        window,
        cx,
    );
}

/// 掩码输入对话框（口令类输入不回显）；空输入不触发确认，可预填默认值
pub fn open_masked_prompt(
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
        true,
        on_confirm,
        window,
        cx,
    );
}

/// 带字节上限的掩码输入；口令保留首尾空格，纯空白仍视为空输入。
#[allow(clippy::too_many_arguments)]
pub fn open_bounded_masked_prompt(
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
        true,
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
        false,
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
        false,
        on_confirm,
        window,
        cx,
    );
}

/// 掩码输入 + 明文显隐切换的口令对话框：免二次输入，用户可切明文自查后提交。
/// `validate` 返回 Some(错误文案) 时阻止提交并内联展示（输入一变即隐藏）；
/// 空输入按「不能为空」处理，口令保留首尾空格
pub fn open_reveal_masked_prompt(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    validate: impl Fn(&str) -> Option<String> + 'static,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = title.into();
    let description: SharedString = description.into();
    let confirm_label: SharedString = confirm_label.into();
    let input: Entity<InputState> = cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|value, _| value.len() <= MAX_PROMPT_INPUT_BYTES)
            .masked(true)
    });
    input.update(cx, |state, cx| state.focus(window, cx));
    let on_confirm_cell = Rc::new(RefCell::new(Some(on_confirm)));
    let validate = Rc::new(validate);
    // (错误文案, 触发时的输入快照)：输入一变即隐藏错误
    let error_state: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));
    let masked_state = Rc::new(RefCell::new(true));

    // 提交检查：空 / 校验失败 → 记录错误保持打开；通过 → 执行回调，返回是否可关闭
    let submit = {
        let cell = on_confirm_cell.clone();
        let input = input.clone();
        let validate = validate.clone();
        let error_state = error_state.clone();
        Rc::new(move |window: &mut Window, app: &mut App| -> bool {
            let value = input.read(app).value().to_string();
            let error = if value.trim().is_empty() {
                Some("输入不能为空".to_string())
            } else {
                validate(&value)
            };
            if let Some(error) = error {
                *error_state.borrow_mut() = Some((error, value));
                input.update(app, |_, cx| cx.notify());
                return false;
            }
            if let Some(cb) = cell.borrow_mut().take() {
                cb(value, window, app);
            }
            true
        })
    };

    window.open_dialog(cx, move |dialog, _, _| {
        let desc = description.clone();
        let confirm_label_inner = confirm_label.clone();

        let cancel_btn = crate::clickable_button("ramag-reveal-prompt-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| {
                window.close_dialog(app);
            });
        let ok_btn = crate::clickable_button("ramag-reveal-prompt-ok")
            .small()
            .primary()
            .label(confirm_label_inner)
            .on_click({
                let submit = submit.clone();
                move |_: &ClickEvent, window, app| {
                    if submit(window, app) {
                        window.close_dialog(app);
                    }
                }
            });

        let input_for_content = input.clone();
        let masked_for_content = masked_state.clone();
        let error_for_content = error_state.clone();
        dialog
            .title(crate::closable_dialog_title(
                "ramag-reveal-prompt-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(180.0))
            .on_ok({
                let submit = submit.clone();
                move |_, window, app| submit(window, app)
            })
            .content(move |content, _, cx| {
                let muted_fg = cx.theme().muted_foreground;
                let masked_now = *masked_for_content.borrow();
                let value = input_for_content.read(cx).value();
                let error_line = error_for_content
                    .borrow()
                    .as_ref()
                    .and_then(|(message, at)| {
                        (value.as_ref() == at.as_str()).then(|| message.clone())
                    });
                let toggle = {
                    let input = input_for_content.clone();
                    let masked_cell = masked_for_content.clone();
                    crate::clickable_button("ramag-reveal-prompt-toggle")
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .icon(if masked_now {
                            IconName::Eye
                        } else {
                            IconName::EyeOff
                        })
                        .tooltip("显示/隐藏")
                        .on_click(move |_: &ClickEvent, window, app| {
                            let next = !*masked_cell.borrow();
                            *masked_cell.borrow_mut() = next;
                            input.update(app, |state, cx| state.set_masked(next, window, cx));
                        })
                };
                content.child(
                    v_flex()
                        .gap(px(8.0))
                        .py(px(4.0))
                        .child(div().text_sm().text_color(muted_fg).child(desc.clone()))
                        .child(Input::new(&input_for_content).small().suffix(toggle))
                        .when_some(error_line, |this, message| {
                            this.child(div().text_xs().text_color(gpui::red()).child(message))
                        }),
                )
            })
            .footer(crate::dialog_action_footer(cancel_btn, ok_btn))
    });
}

#[allow(clippy::too_many_arguments)]
fn open_prompt_impl(
    title: SharedString,
    description: SharedString,
    initial: &str,
    confirm_label: SharedString,
    max_bytes: usize,
    allow_empty: bool,
    masked: bool,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if max_bytes == 0 || initial.len() > max_bytes {
        crate::push_responsive_notification(
            window,
            Notification::error(format!(
                "输入内容超过 {max_bytes} bytes 上限，无法打开编辑对话框"
            )),
            cx,
        );
        return;
    }
    let input: Entity<InputState> = cx.new(|cx| {
        let state = InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes);
        if masked { state.masked(true) } else { state }
    });
    // 预填走运行时 set_value（完整更新路径，掩码输入同样生效）；打开即聚焦无需先点一下
    input.update(cx, |state, cx| {
        if !initial.is_empty() {
            state.set_value(initial, window, cx);
        }
        state.focus(window, cx);
    });
    // FnOnce 包成可 Clone 的 Fn 句柄
    let on_confirm_cell = Rc::new(RefCell::new(Some(on_confirm)));
    // 空输入错误：确认时置位；输入框有内容即自动隐藏，再次清空则复显
    let empty_error = Rc::new(RefCell::new(false));

    window.open_dialog(cx, move |dialog, _, _| {
        let desc = description.clone();
        let confirm_label_inner = confirm_label.clone();

        let cancel_btn = crate::clickable_button("ramag-prompt-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| {
                window.close_dialog(app);
            });

        let ok_btn = crate::clickable_button("ramag-prompt-ok")
            .small()
            .primary()
            .label(confirm_label_inner)
            .on_click({
                let cell = on_confirm_cell.clone();
                let input = input.clone();
                let empty_error_for_btn = empty_error.clone();
                move |_: &ClickEvent, window, app| {
                    let Some(value) = normalize_prompt_value(
                        input.read(app).value().as_ref(),
                        allow_empty,
                        !masked,
                    ) else {
                        *empty_error_for_btn.borrow_mut() = true;
                        input.update(app, |_, cx| cx.notify());
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
            .title(crate::closable_dialog_title(
                "ramag-prompt-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(180.0))
            // 键盘 Enter：与 ok 按钮同逻辑（读输入、空则不关、非空执行）。
            // 返回 false 时对话框不关闭——空输入下回车保持打开，等用户填内容
            .on_ok({
                let cell = on_confirm_cell.clone();
                let input = input.clone();
                let empty_error_for_enter = empty_error.clone();
                move |_, window, app| {
                    let Some(value) = normalize_prompt_value(
                        input.read(app).value().as_ref(),
                        allow_empty,
                        !masked,
                    ) else {
                        *empty_error_for_enter.borrow_mut() = true;
                        input.update(app, |_, cx| cx.notify());
                        return false;
                    };
                    if let Some(cb) = cell.borrow_mut().take() {
                        cb(value, window, app);
                    }
                    true
                }
            })
            .content({
                let empty_error = empty_error.clone();
                move |content, _, cx| {
                    let muted_fg = cx.theme().muted_foreground;
                    let show_empty_error = *empty_error.borrow()
                        && input_for_content.read(cx).value().trim().is_empty();
                    content.child(
                        v_flex()
                            .gap(px(8.0))
                            .py(px(4.0))
                            .child(div().text_sm().text_color(muted_fg).child(desc.clone()))
                            .child(Input::new(&input_for_content).small())
                            .when(show_empty_error, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(gpui::red())
                                        .child("输入不能为空"),
                                )
                            }),
                    )
                }
            })
            .footer(crate::dialog_action_footer(cancel_btn, ok_btn))
    });
}

fn normalize_prompt_value(value: &str, allow_empty: bool, trim_value: bool) -> Option<String> {
    let value = if trim_value { value.trim() } else { value };
    if value.trim().is_empty() && !allow_empty {
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
            normalize_prompt_value("  name  ", false, true).as_deref(),
            Some("name")
        );
        assert_eq!(normalize_prompt_value("   ", false, true), None);
        assert_eq!(
            normalize_prompt_value("   ", true, true).as_deref(),
            Some("")
        );
        assert_eq!(
            normalize_prompt_value("  secret  ", false, false).as_deref(),
            Some("  secret  ")
        );
        assert_eq!(normalize_prompt_value("   ", false, false), None);
    }
}
