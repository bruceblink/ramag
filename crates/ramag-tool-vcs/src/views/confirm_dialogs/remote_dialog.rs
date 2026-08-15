//! 首次推送的远程选择。

use gpui::{ClickEvent, Context, ParentElement, Styled, Window, div, px};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _, button::ButtonVariants as _, h_flex, v_flex,
};

use super::super::helpers::RemoteOp;
use super::super::vcs_view::VcsView;

impl VcsView {
    pub(super) fn open_first_push_remote_picker(
        &mut self,
        op: RemoteOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remotes: Vec<String> = self
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect();
        let branch = self
            .status
            .as_ref()
            .and_then(|status| status.head_branch.clone())
            .unwrap_or_else(|| "当前分支".into());
        let view = cx.entity();
        let force = matches!(op, RemoteOp::PushForce);
        let title = if force {
            "选择强推目标"
        } else {
            "选择推送目标"
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel = ramag_ui::clickable_button("vcs-first-push-cancel")
                .ghost()
                .small()
                .label("取消")
                .on_click(|_: &ClickEvent, window, app| window.close_dialog(app));
            let description = if force {
                "选择强推目标，之后还需确认风险。"
            } else {
                "当前分支未设置 upstream，请选择远程；推送成功后自动设置。"
            };
            dialog
                .title(ramag_ui::closable_dialog_title(
                    "vcs-first-push-close",
                    title,
                    |_, _| {},
                ))
                .close_button(false)
                .width(px(520.0))
                .margin_top(px(150.0))
                .content({
                    let remotes = remotes.clone();
                    let branch = branch.clone();
                    let view = view.clone();
                    move |content, _, cx| {
                        let mut choices = v_flex().w_full().gap(px(6.0));
                        for (index, remote) in remotes.iter().enumerate() {
                            let remote_for_click = remote.clone();
                            let branch_for_click = branch.clone();
                            let view_for_click = view.clone();
                            let button = ramag_ui::clickable_button(format!(
                                "vcs-first-push-remote-{index}"
                            ))
                            .small()
                            .w_full()
                            .label(format!("{remote}/{branch}"))
                            .ghost();
                            choices = choices.child(button.on_click(
                                move |_: &ClickEvent, window, app| {
                                    window.close_dialog(app);
                                    view_for_click.update(app, |this, cx| {
                                        if force {
                                            this.confirm_first_force_push(
                                                remote_for_click.clone(),
                                                branch_for_click.clone(),
                                                window,
                                                cx,
                                            );
                                        } else {
                                            this.run_remote_op_to(
                                                op,
                                                Some(remote_for_click.clone()),
                                                cx,
                                            );
                                        }
                                    });
                                },
                            ));
                        }
                        content.child(
                            v_flex()
                                .w_full()
                                .gap(px(10.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description),
                                )
                                .child(choices),
                        )
                    }
                })
                .footer(h_flex().w_full().items_center().justify_end().child(cancel))
        });
    }

    fn confirm_first_force_push(
        &mut self,
        remote: String,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = format!("{remote}/{branch}");
        let view = cx.entity();
        super::open_confirm_dialog(
            view,
            "确认首次强推？",
            format!(
                "将以 --force-with-lease 强推到 {target}。租约保护不能消除协作者提交丢失的风险。"
            ),
            "强推",
            true,
            move |this, cx| {
                this.run_remote_op_to(RemoteOp::PushForce, Some(remote), cx);
            },
            window,
            cx,
        );
    }
}
