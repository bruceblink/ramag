use super::*;

impl VcsView {
    pub(in crate::views) fn confirm_remote_op(
        &mut self,
        op: RemoteOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let upstream = self
            .local_branches
            .iter()
            .find(|branch| branch.is_head)
            .and_then(|branch| branch.upstream.as_deref());
        let needs_first_push_remote = needs_first_push_remote_picker(op, &self.remotes, upstream);
        if needs_first_push_remote {
            self.open_first_push_remote_picker(op, window, cx);
            return;
        }
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
                    "本地领先 {ahead} 个提交、落后 {behind} 个提交。拉取会合并远程改动，\
                     可能产生合并提交或冲突；如需线性历史，请先获取再变基。"
                ),
                "拉取",
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
            "将以 --force-with-lease 改写远程历史。租约会阻止覆盖意外更新，但协作者的提交仍可能丢失。"
                .into(),
            "强推",
            true,
            move |this, cx| this.run_remote_op(RemoteOp::PushForce, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn confirm_remote_delete(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        open_confirm_dialog(
            view,
            "删除远程仓库？",
            format!("将删除本地远程配置「{name}」，远程服务器不受影响。"),
            "删除",
            true,
            move |this, cx| this.remove_remote_op(name, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn prompt_remote_rename(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = name.clone();
        open_prompt_dialog(
            cx.entity(),
            "重命名远程仓库",
            format!("远程「{name}」的新名称"),
            name,
            "改名",
            MAX_GIT_NAME_ARG_BYTES,
            move |this, new, cx| this.rename_remote_op(old.clone(), new, cx),
            window,
            cx,
        );
    }

    pub(in crate::views) fn prompt_remote_set_url(
        &mut self,
        name: String,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_prompt_dialog(
            cx.entity(),
            "修改远程 URL",
            format!("远程「{name}」的 fetch URL"),
            current,
            "保存",
            MAX_GIT_POSITIONAL_ARG_BYTES,
            move |this, url, cx| this.set_remote_url_op(name.clone(), url, cx),
            window,
            cx,
        );
    }
}
