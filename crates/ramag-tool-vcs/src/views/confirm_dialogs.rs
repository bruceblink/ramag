//! Git 操作确认与输入弹窗。

mod branch_ops;
mod history_ops;
mod remote_dialog;
mod remote_ops;

use gpui::{ClickEvent, Context, Entity, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex,
};
use ramag_domain::entities::{MAX_GIT_NAME_ARG_BYTES, MAX_GIT_POSITIONAL_ARG_BYTES, RepoOperation};

use super::helpers::{
    BranchOp, FileOp, OperationStep, RemoteOp, StashOp, TagOp, default_remote_name,
    needs_first_push_remote_picker,
};
use super::vcs_view::VcsView;

/// 打开确认弹窗并将回调绑定到当前 VCS 视图。
#[allow(clippy::too_many_arguments)]
pub(super) fn open_confirm_dialog(
    view: Entity<VcsView>,
    title: impl Into<SharedString>,
    description: String,
    confirm_label: impl Into<SharedString>,
    danger: bool,
    on_confirm: impl FnOnce(&mut VcsView, &mut Context<VcsView>) + 'static,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    ramag_ui::open_confirm(
        title,
        description,
        confirm_label,
        danger,
        move |_window, app| {
            view.update(app, |this, cx| on_confirm(this, cx));
        },
        window,
        cx,
    );
}

/// 打开单行输入弹窗并将回调绑定到当前 VCS 视图。
#[allow(clippy::too_many_arguments)]
pub(super) fn open_prompt_dialog(
    view: Entity<VcsView>,
    title: impl Into<SharedString>,
    description: String,
    initial: String,
    confirm_label: impl Into<SharedString>,
    max_bytes: usize,
    on_confirm: impl FnOnce(&mut VcsView, String, &mut Context<VcsView>) + 'static,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    ramag_ui::open_bounded_prompt(
        title,
        description,
        &initial,
        confirm_label,
        max_bytes,
        move |value, _window, app| {
            view.update(app, |this, cx| on_confirm(this, value, cx));
        },
        window,
        cx,
    );
}
