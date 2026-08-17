use super::*;

impl VcsView {
    pub(in crate::views) fn confirm_file_op(
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
            format!("将永久丢弃「{path}」的全部未暂存改动。"),
            "丢弃",
            true,
            move |this, cx| this.run_file_op(FileOp::Discard, vec![path_for_run], cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_stash_op(
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
            format!("将永久删除 stash「{stash_msg}」。"),
            "删除",
            true,
            move |this, cx| this.run_stash_op(StashOp::Drop(idx), cx),
            window,
            cx,
        );
    }

    /// 暂存区片段回退可逆；工作区片段丢弃需确认。
    pub(in crate::views) fn confirm_discard_hunk(
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
            "丢弃此改动片段？",
            "将永久丢弃此段未暂存改动。".into(),
            "丢弃",
            true,
            move |this, cx| this.discard_hunk(hunk_idx, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_execute_rebase(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ramag_domain::entities::RebaseAction;
        let total = self.rebase_todos.len();
        let dropped = self
            .rebase_todos
            .iter()
            .filter(|todo| todo.action == RebaseAction::Drop)
            .count();
        let drop_part = if dropped > 0 {
            format!("，其中 {dropped} 个将被永久丢弃")
        } else {
            String::new()
        };
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "执行交互式 Rebase？",
            format!("将改写 {total} 个提交{drop_part}；若已推送，之后需强推。"),
            "执行",
            true,
            move |this, cx| this.execute_interactive_rebase(cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_op_step(
        &mut self,
        step: OperationStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(step, OperationStep::Continue) {
            self.run_op_step(step, cx);
            return;
        }
        if matches!(step, OperationStep::Skip) {
            let view = cx.entity();
            open_confirm_dialog(
                view,
                "跳过当前提交？",
                "当前提交不会进入 Rebase 结果，并将继续处理下一条。".into(),
                "跳过",
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
            format!("将中止「{op_name}」并回到操作前；尚未提交的冲突处理结果将丢失。"),
            "中止",
            true,
            move |this, cx| this.run_op_step(OperationStep::Abort, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_amend {
            self.run_commit(cx);
            return;
        }
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "修订上一次提交？",
            "将用当前暂存区和提交说明替换上一次提交；若已推送，之后需强推。".into(),
            "修订",
            false,
            move |this, cx| this.run_commit(cx),
            window,
            cx,
        );
    }
}
