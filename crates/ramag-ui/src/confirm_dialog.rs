//! 破坏性操作二次确认。danger=true 红、false primary 蓝；on_confirm 仅触发一次

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, ClickEvent, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex,
};

pub fn open_confirm(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    danger: bool,
    on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = title.into();
    let description: SharedString = description.into();
    let confirm_label: SharedString = confirm_label.into();
    // FnOnce 包成可 Clone 的 Fn 句柄
    let on_confirm_cell = Rc::new(RefCell::new(Some(on_confirm)));

    window.open_dialog(cx, move |dialog, _, _| {
        let desc = description.clone();
        let confirm_label_inner = confirm_label.clone();

        let cancel_btn = crate::clickable_button("ramag-confirm-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| {
                window.close_dialog(app);
            });

        let mut ok_btn = crate::clickable_button("ramag-confirm-ok")
            .small()
            .label(confirm_label_inner);
        ok_btn = if danger {
            ok_btn.danger()
        } else {
            ok_btn.primary()
        };

        let ok_btn = ok_btn.on_click({
            let cell = on_confirm_cell.clone();
            move |_: &ClickEvent, window, app| {
                if let Some(cb) = cell.borrow_mut().take() {
                    cb(window, app);
                }
                window.close_dialog(app);
            }
        });

        dialog
            .title(crate::closable_dialog_title(
                "ramag-confirm-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(180.0))
            // 键盘 Enter 走 ConfirmDialog action → button_props.on_ok；设了 footer 后
            // 库会忽略 button_props 的按钮渲染，但 on_ok 仍是 Enter 的回调，必须显式绑定，
            // 否则回车只关窗不执行确认（返回 true = 执行后关闭对话框）
            .on_ok({
                let cell = on_confirm_cell.clone();
                move |_, window, app| {
                    if let Some(cb) = cell.borrow_mut().take() {
                        cb(window, app);
                    }
                    true
                }
            })
            .content(move |content, _, cx| {
                let muted_fg = cx.theme().muted_foreground;
                content.child(
                    div()
                        .py(px(4.0))
                        .text_sm()
                        .text_color(muted_fg)
                        .child(desc.clone()),
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
