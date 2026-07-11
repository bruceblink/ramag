//! 破坏性操作二次确认。click 统一走 `confirm_xxx`，非破坏性直转 run，破坏性弹 dialog。
//! 删 / 强推 / 丢工作 用 danger 红；合并 / rebase / amend 用 primary 蓝

use gpui::{ClickEvent, Context, Entity, ParentElement, SharedString, Styled, Window, div, px};
use gpui_component::{
    ActiveTheme, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use ramag_domain::entities::RepoOperation;

use super::helpers::{BranchOp, FileOp, RemoteOp, StashOp, TagOp, default_remote_name};
use super::vcs_view::VcsView;

/// 委托 `ramag_ui::open_confirm`，把 `FnOnce(&mut VcsView, &mut Context)` 适配成 `FnOnce(&mut Window, &mut App)`
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

impl VcsView {
    /// File 操作（Stage / Unstage / Discard）；只有 Discard 弹确认
    pub(super) fn confirm_file_op(
        &mut self,
        op: FileOp,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(op, FileOp::Discard) {
            self.run_file_op(op, vec![path], cx);
            return;
        }
        let view = cx.entity();
        let path_for_run = path.clone();
        open_confirm_dialog(
            view,
            "丢弃工作区改动？",
            format!("将丢弃「{path}」在工作区的全部未暂存改动，且无法恢复。\n确认继续吗？"),
            "丢弃",
            true,
            move |this, cx| this.run_file_op(FileOp::Discard, vec![path_for_run], cx),
            window,
            cx,
        );
    }

    /// Stash 操作；Drop 弹确认（Save / Apply / Pop 不弹——内容仍在工作区可恢复）
    pub(super) fn confirm_stash_op(
        &mut self,
        op: StashOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StashOp::Drop(idx) = op else {
            self.run_stash_op(op, cx);
            return;
        };
        let stash_msg = self
            .stashes
            .get(idx)
            .map(|s| s.message.clone())
            .unwrap_or_else(|| format!("stash@{{{idx}}}"));
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "删除 stash？",
            format!("将永久删除 stash「{stash_msg}」，无法恢复。\n确认继续吗？"),
            "删除",
            true,
            move |this, cx| this.run_stash_op(StashOp::Drop(idx), cx),
            window,
            cx,
        );
    }

    /// Remote 同步操作；只有 PushForce 弹确认（fetch/pull/push 普通操作不弹）
    pub(super) fn confirm_remote_op(
        &mut self,
        op: RemoteOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let diverged_pull = matches!(op, RemoteOp::Pull)
            && self.status.as_ref().is_some_and(|status| {
                status.ahead.unwrap_or(0) > 0 && status.behind.unwrap_or(0) > 0
            });
        if diverged_pull {
            let ahead = self.status.as_ref().and_then(|s| s.ahead).unwrap_or(0);
            let behind = self.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
            let view = cx.entity();
            open_confirm_dialog(
                view,
                "本地与远程已分叉",
                format!(
                    "本地领先 {ahead} 个 commit，同时落后 {behind} 个 commit。\n\n\
                     Pull 会把远程改动合并到当前分支，可能产生 merge commit 或冲突；\
                     如希望线性历史，可取消后先 Fetch，再选择 Rebase。"
                ),
                "Pull 并合并",
                false,
                move |this, cx| this.run_remote_op(RemoteOp::Pull, cx),
                window,
                cx,
            );
            return;
        }
        if !matches!(op, RemoteOp::PushForce) {
            self.run_remote_op(op, cx);
            return;
        }
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "强制推送？",
            "git push --force-with-lease 会改写远程分支历史。\n\
             仅当远程的 ref 仍指向你预期的 commit 时才覆盖（比 --force 安全），\
             但仍可能让其他基于此分支工作的协作者丢失提交。\n确认继续吗？"
                .into(),
            "强推",
            true,
            move |this, cx| this.run_remote_op(RemoteOp::PushForce, cx),
            window,
            cx,
        );
    }

    /// hunk 回滚：Staged→unstage 可逆直接执行；Unstaged→discard 不可恢复，先确认
    pub(super) fn confirm_discard_hunk(
        &mut self,
        hunk_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_changes_kind_is_staged() {
            self.discard_hunk(hunk_idx, cx);
            return;
        }
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "丢弃此 hunk？",
            "将丢弃这一段（hunk）在工作区的未暂存改动，且无法恢复。\n确认继续吗？".into(),
            "丢弃",
            true,
            move |this, cx| this.discard_hunk(hunk_idx, cx),
            window,
            cx,
        );
    }

    /// 交互式 rebase 执行前确认：改写历史 + Drop 不可恢复 + 已推送需强推
    pub(super) fn confirm_execute_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use ramag_domain::entities::RebaseAction;
        let total = self.rebase_todos.len();
        let dropped = self
            .rebase_todos
            .iter()
            .filter(|todo| todo.action == RebaseAction::Drop)
            .count();
        let drop_part = if dropped > 0 {
            format!("，其中 {dropped} 个将被丢弃（不可恢复）")
        } else {
            String::new()
        };
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "执行交互式 Rebase？",
            format!(
                "将按计划改写 {total} 个 commit 的历史{drop_part}。\n\
                 改写后的 commit 会获得新的 hash；若这些 commit 已推送到远程，完成后需要强推。\n\
                 确认执行吗？"
            ),
            "执行 Rebase",
            true,
            move |this, cx| this.execute_interactive_rebase(cx),
            window,
            cx,
        );
    }

    /// 分支操作；Delete / Merge / Rebase 弹确认（Checkout / Create 不弹）
    /// reflog checkout 到 commit（detached HEAD）：脏工作区先走 stash/discard 引导（同分支 checkout），
    /// 干净则直接 checkout_reflog_entry（保留切回 commit 历史的行为）
    pub(super) fn confirm_checkout_reflog(
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

    pub(super) fn confirm_branch_op(
        &mut self,
        op: BranchOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, desc, btn, danger) = match &op {
            BranchOp::Delete(name, true) => (
                "强制删除分支？",
                format!(
                    "分支「{name}」可能还有未合并的提交，强制删除后这些提交不再可达，可能丢失。\n\
                     确认继续吗？"
                ),
                "强制删除",
                true,
            ),
            BranchOp::Delete(name, false) => (
                "删除分支？",
                format!("将删除本地分支「{name}」（仅当已合并；未合并会报错）。\n确认继续吗？"),
                "删除",
                true,
            ),
            BranchOp::Merge(name) => (
                "合并分支？",
                format!(
                    "将「{name}」合并到当前 HEAD（默认 --no-ff，建 merge commit）。\n\
                     有冲突时会进入合并进行中状态，需手动解决后再继续。"
                ),
                "合并",
                false,
            ),
            BranchOp::Rebase(name) => (
                "Rebase 到目标分支？",
                format!(
                    "将当前分支 rebase 到「{name}」上：\n\
                     - 改写当前分支的 commit 历史\n\
                     - 已 push 的分支谨慎使用（推送时需要 --force-with-lease）\n\
                     有冲突时会进入 rebase 进行中状态。"
                ),
                "Rebase",
                false,
            ),
            BranchOp::Checkout(name) => {
                // dirty（含 untracked）时引导 stash/discard
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

    /// Tag 操作；Delete / Push 弹确认（Create 不弹）
    pub(super) fn confirm_tag_op(
        &mut self,
        op: TagOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, desc, btn, danger) = match &op {
            TagOp::Delete(name) => (
                "删除 tag？",
                format!("将删除本地 tag「{name}」。已推送的 tag 删除后远程仍存在。\n确认继续吗？"),
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
                    format!("将把 tag「{name}」推送到 {remote}。\n推送后此 tag 对所有协作者可见。"),
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

    /// 进行中操作的步进；Continue 直接执行，Skip / Abort 涉及丢弃内容须确认。
    pub(super) fn confirm_op_step(
        &mut self,
        step: super::helpers::OperationStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::helpers::OperationStep;
        if matches!(step, OperationStep::Continue) {
            self.run_op_step(step, cx);
            return;
        }
        if matches!(step, OperationStep::Skip) {
            let view = cx.entity();
            open_confirm_dialog(
                view,
                "跳过当前 Rebase commit？",
                "将丢弃当前正在重放的 commit，并继续处理下一条。\n该 commit 的改动不会进入 Rebase 结果，确认继续吗？"
                    .into(),
                "跳过 commit",
                true,
                move |this, cx| this.run_op_step(OperationStep::Skip, cx),
                window,
                cx,
            );
            return;
        }
        let op_name = self
            .status
            .as_ref()
            .and_then(|s| s.operation)
            .map(|o| match o {
                RepoOperation::Merge => "合并",
                RepoOperation::Rebase => "Rebase",
                RepoOperation::CherryPick => "Cherry-pick",
                RepoOperation::Revert => "Revert",
            })
            .unwrap_or("当前操作");
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "中止当前操作？",
            format!(
                "将中止进行中的「{op_name}」并回到操作前的状态。\n\
                 已解决一半的冲突会被丢弃，无法恢复。"
            ),
            "中止",
            true,
            move |this, cx| this.run_op_step(OperationStep::Abort, cx),
            window,
            cx,
        );
    }

    /// Commit：amend=true 时弹确认（普通 commit 不弹）
    pub(super) fn confirm_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_amend {
            self.run_commit(cx);
            return;
        }
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "Amend 上一次提交？",
            "将用当前暂存区改动 + 新 message 替换最近一次 commit。\n\
             如果该 commit 已经 push 到远程，再次推送需要 --force-with-lease。"
                .into(),
            "Amend 提交",
            false,
            move |this, cx| this.run_commit(cx),
            window,
            cx,
        );
    }
}

/// 工作区脏 + checkout：取消 / Stash 后切（不自动 pop）/ Discard 后切（红色危险）
pub(super) fn open_checkout_dirty_dialog(
    view: Entity<VcsView>,
    target: String,
    window: &mut Window,
    cx: &mut Context<VcsView>,
) {
    let title = SharedString::from("工作区有未提交改动");
    let desc = format!(
        "工作区有未提交改动。直接切换到「{target}」可能被 Git 拒绝，也可能把改动带到目标分支。\n\n\
         - 「Stash 后切换」：把当前改动（含未跟踪文件）暂存到 stash 列表，切换后可在 Stash 面板恢复（推荐）\n\
         - 「丢弃后切换」：先临时保护改动，确认切换成功后再永久删除该备份（不可恢复）"
    );
    window.open_dialog(cx, move |dialog, _, _| {
        let view_cancel = view.clone();
        let view_stash = view.clone();
        let view_discard = view.clone();
        let target_stash = target.clone();
        let target_discard = target.clone();
        let desc = desc.clone();
        dialog
            .title(title.clone())
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
                        Button::new("vcs-co-cancel")
                            .ghost()
                            .small()
                            .label("取消")
                            .on_click({
                                let _ = view_cancel;
                                |_: &ClickEvent, w, app| w.close_dialog(app)
                            }),
                    )
                    .child(
                        Button::new("vcs-co-discard")
                            .danger()
                            .small()
                            .label("丢弃后切换")
                            .tooltip("切换成功后永久丢弃当前全部改动（含未跟踪文件）")
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
                        Button::new("vcs-co-stash")
                            .primary()
                            .small()
                            .label("Stash 后切换")
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
