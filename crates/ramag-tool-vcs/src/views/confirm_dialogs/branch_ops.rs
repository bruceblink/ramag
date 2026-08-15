use super::*;

impl VcsView {
    /// 脏工作区切换前提供储藏或丢弃选择。
    pub(in crate::views) fn confirm_checkout_reflog(
        &mut self,
        commit: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_working_tree_dirty() {
            open_checkout_dirty_dialog(cx.entity(), commit, window, cx);
        } else {
            self.checkout_reflog_entry(commit, cx);
        }
    }

    pub(in crate::views) fn confirm_branch_op(
        &mut self,
        op: BranchOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, desc, btn, danger) = match &op {
            BranchOp::Delete(name, true) => (
                "强制删除分支？",
                format!("分支「{name}」可能含未合并提交，强制删除后这些提交可能无法恢复。"),
                "强制删除",
                true,
            ),
            BranchOp::Delete(name, false) => (
                "删除分支？",
                format!("将删除已合并的本地分支「{name}」；未合并时操作会失败。"),
                "删除",
                true,
            ),
            BranchOp::Merge(name) => (
                "合并分支？",
                format!("将「{name}」以 --no-ff 合并到当前分支；发生冲突时需解决后继续。"),
                "合并",
                false,
            ),
            BranchOp::Rebase(name) => (
                "变基到目标分支？",
                format!(
                    "将当前分支变基到「{name}」并改写提交历史。若已推送，之后需要强推；发生冲突时需手动处理。"
                ),
                "变基",
                false,
            ),
            BranchOp::Checkout(name) => {
                if self.is_working_tree_dirty() {
                    open_checkout_dirty_dialog(cx.entity(), name.clone(), window, cx);
                } else {
                    self.run_branch_op(op, cx);
                }
                return;
            }
            _ => {
                self.run_branch_op(op, cx);
                return;
            }
        };
        let view = cx.entity();
        open_confirm_dialog(
            view,
            title,
            desc,
            btn,
            danger,
            move |this, cx| this.run_branch_op(op, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_tag_op(
        &mut self,
        op: TagOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, desc, btn, danger) = match &op {
            TagOp::Delete(name) => (
                "删除 tag？",
                format!("将删除本地 tag「{name}」；远程同名 tag 不受影响。"),
                "删除",
                true,
            ),
            TagOp::Push(name) => {
                let remote = match default_remote_name(&self.remotes) {
                    Ok(remote) => remote,
                    Err(message) => {
                        self.error = Some(message);
                        cx.notify();
                        return;
                    }
                };
                (
                    "推送 tag 到远程？",
                    format!("将 tag「{name}」推送到 {remote}。"),
                    "推送",
                    false,
                )
            }
            _ => {
                self.run_tag_op(op, cx);
                return;
            }
        };
        let view = cx.entity();
        open_confirm_dialog(
            view,
            title,
            desc,
            btn,
            danger,
            move |this, cx| this.run_tag_op(op, cx),
            window,
            cx,
        );
    }
}

/// 提供储藏后切换或丢弃后切换。
pub(super) fn open_checkout_dirty_dialog(
    view: Entity<VcsView>,
    target: String,
    window: &mut Window,
    cx: &mut Context<VcsView>,
) {
    let title = SharedString::from("工作区有未提交改动");
    let desc = format!(
        "切换到「{target}」前请选择如何处理当前改动（含未跟踪文件）：\n\n\
         - 「储藏并切换」：保存到 Stash，稍后可恢复\n\
         - 「丢弃并切换」：切换成功后永久删除备份"
    );
    window.open_dialog(cx, move |dialog, _, _| {
        let view_cancel = view.clone();
        let view_stash = view.clone();
        let view_discard = view.clone();
        let target_stash = target.clone();
        let target_discard = target.clone();
        let desc = desc.clone();
        dialog
            .title(ramag_ui::closable_dialog_title(
                "vcs-checkout-dirty-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(180.0))
            .content(move |c, _, cx| {
                let muted_fg = cx.theme().muted_foreground;
                c.child(
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
                    .child(
                        ramag_ui::clickable_button("vcs-co-cancel")
                            .ghost()
                            .small()
                            .label("取消")
                            .on_click({
                                let _ = view_cancel;
                                |_: &ClickEvent, w, app| w.close_dialog(app)
                            }),
                    )
                    .child(
                        ramag_ui::clickable_button("vcs-co-discard")
                            .danger()
                            .small()
                            .label("丢弃并切换")
                            .on_click({
                                let v = view_discard.clone();
                                move |_: &ClickEvent, w, app| {
                                    let target = target_discard.clone();
                                    v.update(app, |this, cx| {
                                        this.run_checkout_with_discard(target, cx);
                                    });
                                    w.close_dialog(app);
                                }
                            }),
                    )
                    .child(
                        ramag_ui::clickable_button("vcs-co-stash")
                            .primary()
                            .small()
                            .label("储藏并切换")
                            .on_click({
                                let v = view_stash.clone();
                                move |_: &ClickEvent, w, app| {
                                    let target = target_stash.clone();
                                    v.update(app, |this, cx| {
                                        this.run_checkout_with_stash(target, cx);
                                    });
                                    w.close_dialog(app);
                                }
                            }),
                    ),
            )
    });
}
