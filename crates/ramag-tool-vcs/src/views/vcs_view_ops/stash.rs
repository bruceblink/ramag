//! VcsView Stash 异步操作：加载列表 + save / apply / pop / drop

use gpui::Context;
use tracing::error;

use super::super::helpers::{FilesViewMode, StashOp};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// 异步加载 stash 列表（仓库打开时 + stash 操作完成后调用）
    pub(in crate::views) fn reload_stashes(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_stashes = true;
        self.stash_request_seq = self.stash_request_seq.wrapping_add(1);
        let request_seq = self.stash_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_stashes(&repo).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.stash_request_seq != request_seq {
                    return;
                }
                this.loading_stashes = false;
                match result {
                    Ok(list) => this.stashes = list,
                    Err(e) => {
                        error!(
                            operation = "git_stash_list",
                            repo_id = %repo,
                            error = %e,
                            "load stashes failed"
                        );
                        this.error = Some(format!("加载 Stash 列表失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 主动 stash 当前工作区改动（含 untracked）；message 为空用 git 默认描述
    pub(in crate::views) fn run_stash_save(&mut self, message: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("保存 Stash", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Stash 中…", cx) {
            return;
        }
        self.stash_request_seq = self.stash_request_seq.wrapping_add(1);
        self.loading_stashes = false;

        cx.spawn(async move |this, cx| {
            let msg = message.trim().to_string();
            let msg_opt = (!msg.is_empty()).then_some(msg.as_str());
            let result = driver.stash_save(&repo, msg_opt, true).await;
            let new_stashes = driver.list_stashes(&repo).await;
            let new_status = driver.status(&repo).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                if let Ok(list) = &new_stashes {
                    this.stashes = list.clone();
                }
                if let Ok(status) = &new_status {
                    this.status = Some(status.clone());
                }
                match result {
                    Err(e) => {
                        error!(
                            operation = "git_stash_save",
                            repo_id = %repo,
                            error = %e,
                            "stash save failed"
                        );
                        this.error = Some(format!("Stash 失败：{e}"));
                    }
                    Ok(_) => {
                        // 工作区被清空 → 已开的 Changes tabs 全部失效
                        this.sync_changes_tabs_with_status(cx);
                        // stash 会带走 untracked 文件，Project Files 视图同步刷新
                        if matches!(this.files_view_mode, FilesViewMode::Project) {
                            this.reload_project_files(cx);
                        }
                        this.notify_success("已 stash 工作区改动（含未跟踪文件）", cx);
                        if let Err(e) = &new_stashes {
                            this.error = Some(format!("Stash 已完成，但刷新列表失败：{e}"));
                        } else if let Err(e) = &new_status {
                            this.error = Some(format!("Stash 已完成，但刷新工作区状态失败：{e}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stash 操作：保存 / 应用 / 弹出 / 删除
    pub(in crate::views) fn run_stash_op(&mut self, op: StashOp, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("操作 Stash", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Stash 操作中…", cx) {
            return;
        }
        self.stash_request_seq = self.stash_request_seq.wrapping_add(1);
        self.loading_stashes = false;

        cx.spawn(async move |this, cx| {
            let result = match op {
                StashOp::Apply(idx) => driver.stash_apply(&repo, idx, false).await,
                StashOp::Pop(idx) => driver.stash_apply(&repo, idx, true).await,
                StashOp::Drop(idx) => driver.stash_drop(&repo, idx).await,
            };
            // 操作后刷新 stashes + status
            let new_stashes = driver.list_stashes(&repo).await;
            if let Err(error) = &new_stashes {
                tracing::warn!(
                    operation = "git_stash_refresh",
                    repo_id = %repo,
                    resource = "stashes",
                    error = %error,
                    "stash list refresh failed"
                );
            }
            let new_status = driver.status(&repo).await;
            if let Err(error) = &new_status {
                tracing::warn!(
                    operation = "git_stash_refresh",
                    repo_id = %repo,
                    resource = "workspace_status",
                    error = %error,
                    "workspace status refresh failed after stash"
                );
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                if let Ok(list) = &new_stashes {
                    this.stashes = list.clone();
                }
                if let Ok(status) = &new_status {
                    this.status = Some(status.clone());
                }
                let conflict_count = new_status
                    .as_ref()
                    .ok()
                    .map(|status| status.files.iter().filter(|file| file.is_conflicted()).count())
                    .unwrap_or(0);
                match result {
                    Err(e)
                        if conflict_count > 0
                            && matches!(op, StashOp::Apply(_) | StashOp::Pop(_)) =>
                    {
                        tracing::info!(
                            operation = "git_stash_operation",
                            repo_id = %repo,
                            stash_operation = ?op,
                            conflict_count,
                            error = %e,
                            "stash apply paused on conflict"
                        );
                        this.error = None;
                        this.view_mode = super::super::helpers::ViewMode::Workspace;
                        this.files_view_mode = FilesViewMode::Changes;
                        this.sync_changes_tabs_with_status(cx);
                        this.notify_warning(
                            format!(
                                "Stash 已应用但有 {conflict_count} 个冲突；原 stash 条目已保留，请在 Changes 中解决"
                            ),
                            cx,
                        );
                    }
                    Err(e) => {
                        error!(
                            operation = "git_stash_operation",
                            repo_id = %repo,
                            stash_operation = ?op,
                            error = %e,
                            "stash operation failed"
                        );
                        this.error = Some(format!("Stash 操作失败：{e}"));
                    }
                    Ok(_) => {
                        tracing::info!(
                            operation = "git_stash_operation",
                            repo_id = %repo,
                            stash_operation = ?op,
                            "stash operation completed"
                        );
                        // apply / pop 会改工作区文件 → tabs 对齐
                        if matches!(op, StashOp::Apply(_) | StashOp::Pop(_)) {
                            this.sync_changes_tabs_with_status(cx);
                        }
                        // apply / pop 可能还原 untracked 文件，Project Files 视图同步刷新
                        if matches!(op, StashOp::Apply(_) | StashOp::Pop(_))
                            && matches!(this.files_view_mode, FilesViewMode::Project)
                        {
                            this.reload_project_files(cx);
                        }
                        let msg = match op {
                            StashOp::Apply(_) => "已应用 stash（保留堆栈条目）",
                            StashOp::Pop(_) => "已弹出 stash 到工作区",
                            StashOp::Drop(_) => "已删除 stash",
                        };
                        this.notify_success(msg, cx);
                        if let Err(e) = &new_stashes {
                            this.error = Some(format!("Stash 操作已完成，但刷新列表失败：{e}"));
                        } else if let Err(e) = &new_status {
                            this.error =
                                Some(format!("Stash 操作已完成，但刷新工作区状态失败：{e}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
