//! VCS 远程操作。

use gpui::Context;
use ramag_domain::entities::RepoId;
use tracing::{error, info};

use super::super::helpers::{RemoteOp, default_remote_name, is_current_arc_slot};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// 执行 Fetch、Pull 或 Push。
    pub(in crate::views) fn run_remote_op(&mut self, op: RemoteOp, cx: &mut Context<Self>) {
        self.run_remote_op_to(op, None, cx);
    }

    /// 已有上游时始终使用上游；仅首次推送使用选择的远端。
    pub(in crate::views) fn run_remote_op_to(
        &mut self,
        op: RemoteOp,
        selected_remote: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(op, RemoteOp::Fetch)
            && self
                .status
                .as_ref()
                .and_then(|status| status.head_commit.as_ref())
                .is_none()
        {
            self.error = Some("首次提交前没有可 Pull / Push 的分支历史；请先创建 commit".into());
            cx.notify();
            return;
        }
        if !matches!(op, RemoteOp::Fetch)
            && let Some(operation) = self.status.as_ref().and_then(|status| status.operation)
        {
            self.error = Some(format!(
                "{}仍在进行中：完成或中止后再执行 Pull / Push",
                super::super::helpers::operation_label(operation)
            ));
            cx.notify();
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let local_branch = self.status.as_ref().and_then(|s| s.head_branch.clone());
        if local_branch.is_none() && !matches!(op, RemoteOp::Fetch) {
            self.error = Some("当前为 detached HEAD，无法 push/pull".into());
            cx.notify();
            return;
        }
        // Fetch 无需 HEAD。
        let local_branch = local_branch.unwrap_or_default();
        let upstream = self
            .local_branches
            .iter()
            .find(|b| b.is_head)
            .and_then(|b| b.upstream.clone());
        let need_set_upstream = upstream.is_none();
        // Pull 必须跟踪上游。
        if matches!(op, RemoteOp::Pull) && need_set_upstream {
            self.error =
                Some("当前分支没有上游分支：先点 Push（会自动设置 upstream）再 Pull".into());
            cx.notify();
            return;
        }
        let (remote_name, remote_branch) = match upstream.as_deref().and_then(|u| u.split_once('/'))
        {
            Some((r, b)) => (r.to_string(), b.to_string()),
            None if matches!(op, RemoteOp::Push | RemoteOp::PushForce) => {
                let remote = match selected_remote {
                    Some(remote) if self.remotes.iter().any(|item| item.name == remote) => remote,
                    Some(remote) => {
                        self.error = Some(format!("远程仓库「{remote}」已不存在，请重新选择"));
                        cx.notify();
                        return;
                    }
                    None => match default_remote_name(&self.remotes) {
                        Ok(remote) => remote,
                        Err(message) => {
                            self.error = Some(message);
                            cx.notify();
                            return;
                        }
                    },
                };
                (remote, local_branch.clone())
            }
            None => (String::new(), local_branch.clone()),
        };
        // 仅强推使用 force-with-lease。
        let this_force_lease = matches!(op, RemoteOp::PushForce);
        let driver = self.driver.clone();
        // 用于区分传输与已最新。
        let pre_ahead = self.status.as_ref().and_then(|s| s.ahead).unwrap_or(0);
        let pre_behind = self.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
        let op_label = match op {
            RemoteOp::Fetch => "Fetch",
            RemoteOp::Pull => "Pull",
            RemoteOp::Push => "Push",
            RemoteOp::PushForce => "强推",
        };
        let label = match op {
            RemoteOp::Fetch => "Fetch 中…",
            RemoteOp::Pull => "Pull 中…",
            RemoteOp::Push => "Push 中…",
            RemoteOp::PushForce => "强推中…",
        };
        if !self.begin_op(label, cx) {
            return;
        }
        // 保存进度与取消位。
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.remote_op_cancel = Some(cancel.clone());
        self.remote_op_progress = Some(progress.clone());
        // 定期刷新工具栏进度。
        let poll_cancel = cancel.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(120))
                    .await;
                let still = this
                    .update(cx, |this, cx| {
                        let active =
                            is_current_arc_slot(this.remote_op_cancel.as_ref(), &poll_cancel);
                        if active {
                            cx.notify();
                        }
                        active
                    })
                    .unwrap_or(false);
                if !still {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let result = match op {
                RemoteOp::Fetch => {
                    driver
                        .fetch_streaming(&repo, "", cancel.clone(), progress.clone())
                        .await
                }
                RemoteOp::Pull => {
                    driver
                        .pull_streaming(
                            &repo,
                            &remote_name,
                            &remote_branch,
                            false,
                            cancel.clone(),
                            progress.clone(),
                        )
                        .await
                }
                RemoteOp::Push | RemoteOp::PushForce => {
                    driver
                        .push_streaming(
                            &repo,
                            &remote_name,
                            &local_branch,
                            need_set_upstream,
                            this_force_lease,
                            cancel.clone(),
                            progress.clone(),
                        )
                        .await
                }
            };
            // 完成后刷新状态和远端分支。
            let (new_status, branches) = futures::future::join(
                driver.status(&repo),
                driver.list_all_branches(&repo),
            )
            .await;
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                new_status,
                &repo,
                "workspace status",
            );
            let branches = crate::views::vcs_view_ops_sync::best_effort_refresh(
                branches,
                &repo,
                "branches",
            );
            let _ = this.update(cx, |this, cx| {
                // 仅发起任务可收尾。
                if !is_current_arc_slot(this.remote_op_cancel.as_ref(), &cancel) {
                    return;
                }
                this.busy = false;
                this.busy_label = None;
                // 清除进度和取消状态。
                let was_cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
                this.remote_op_cancel = None;
                this.remote_op_progress = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                // 用户取消显示中性提示。
                if was_cancelled {
                    if let Some(s) = new_status {
                        this.status = Some(s);
                    }
                    this.notify_warning(format!("已取消 {op_label}"), cx);
                    cx.notify();
                    return;
                }
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                if let Some((local, remote)) = branches {
                    this.local_branches = local;
                    this.remote_branches = remote;
                }
                let paused = matches!(op, RemoteOp::Pull)
                    .then(|| this.status.as_ref().and_then(|s| s.operation))
                    .flatten();
                match (result, paused) {
                    (_, Some(operation)) => {
                        info!(
                            operation = "git_remote_operation",
                            repo_id = %repo,
                            remote_operation = ?op,
                            paused_operation = ?operation,
                            "pull paused"
                        );
                        this.handle_operation_paused(operation, cx);
                        this.refresh_after_head_change(cx);
                    }
                    (Err(e), None) => {
                        error!(
                            operation = "git_remote_operation",
                            repo_id = %repo,
                            remote_operation = ?op,
                            error = %e,
                            "remote operation failed"
                        );
                        this.error = Some(format!("{op_label} 失败：{e}"));
                    }
                    (Ok(_), None) => {
                        info!(
                            operation = "git_remote_operation",
                            repo_id = %repo,
                            remote_operation = ?op,
                            remote = %remote_name,
                            branch = %remote_branch,
                            "remote operation completed"
                        );
                        let post_behind =
                            this.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
                        let msg = match op {
                            RemoteOp::Fetch if post_behind > pre_behind => {
                                format!("Fetch 完成：发现 {} 个新 commit", post_behind - pre_behind)
                            }
                            RemoteOp::Fetch => "Fetch 完成：远程引用已更新".to_string(),
                            RemoteOp::Pull if pre_behind == 0 => {
                                format!("Pull 完成：已同步 {remote_name}/{remote_branch}")
                            }
                            RemoteOp::Pull => format!(
                                "Pull 完成：合入 {pre_behind} 个 commit（{remote_name}/{remote_branch}）"
                            ),
                            RemoteOp::Push if need_set_upstream => {
                                format!("Push 成功，已设置 upstream {remote_name}/{local_branch}")
                            }
                            RemoteOp::Push if pre_ahead == 0 => {
                                format!("Push 完成：已同步 {remote_name}/{local_branch}")
                            }
                            RemoteOp::Push => format!(
                                "Push 成功：已推送 {pre_ahead} 个 commit（{remote_name}/{local_branch}）"
                            ),
                            RemoteOp::PushForce => {
                                format!("强推成功（{remote_name}/{local_branch}）")
                            }
                        };
                        this.notify_success(msg, cx);
                        // Pull 后刷新缓存和历史。
                        if matches!(op, RemoteOp::Pull) {
                            this.refresh_after_head_change(cx);
                            if this.history_pane_visible || !this.history_commits.is_empty() {
                                this.load_history_page(0, cx);
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 请求取消正在执行的远端操作。
    pub(in crate::views) fn cancel_remote_op(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.remote_op_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.busy_label = Some("取消中…");
            tracing::info!(
                operation = "git_remote_cancel",
                repo_id = ?self.repo.as_ref().map(|repo| &repo.id),
                "remote operation cancellation requested"
            );
            cx.notify();
        }
    }

    /// 返回当前远端操作进度。
    pub(in crate::views) fn remote_op_progress_line(&self) -> Option<String> {
        let slot = self.remote_op_progress.as_ref()?;
        let text = match slot.try_lock() {
            Ok(text) => text.clone(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(error)) => {
                tracing::warn!(
                    operation = "git_remote_progress",
                    "remote progress lock poisoned"
                );
                error.into_inner().clone()
            }
        };
        if text.is_empty() { None } else { Some(text) }
    }

    /// 校验并添加远端。
    pub(in crate::views) fn handle_create_remote(&mut self, cx: &mut Context<Self>) {
        let name = self
            .create_remote_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let url = self
            .create_remote_url_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() || url.is_empty() {
            self.error = Some("远程名与 URL 均不能为空".into());
            cx.notify();
            return;
        }
        self.add_remote_op(name, url, cx);
    }

    /// 添加远端并刷新列表。
    pub(in crate::views) fn add_remote_op(
        &mut self,
        name: String,
        url: String,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("添加远程中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.add_remote(&repo, &name, &url).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_remote_crud(&repo, result, format!("已添加远程 {name}"), true, cx);
            });
        })
        .detach();
    }

    pub(in crate::views) fn remove_remote_op(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("删除远程中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.remove_remote(&repo, &name).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_remote_crud(&repo, result, format!("已删除远程 {name}"), false, cx);
            });
        })
        .detach();
    }

    /// 修改远端 URL。
    pub(in crate::views) fn set_remote_url_op(
        &mut self,
        name: String,
        url: String,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("修改远程 URL 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.set_remote_url(&repo, &name, &url).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_remote_crud(
                    &repo,
                    result,
                    format!("已更新远程 {name} 的 URL"),
                    false,
                    cx,
                );
            });
        })
        .detach();
    }

    pub(in crate::views) fn rename_remote_op(
        &mut self,
        old: String,
        new: String,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("重命名远程中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.rename_remote(&repo, &old, &new).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_remote_crud(
                    &repo,
                    result,
                    format!("已将远程 {old} 重命名为 {new}"),
                    false,
                    cx,
                );
            });
        })
        .detach();
    }

    /// 统一完成远端配置操作。
    fn finish_remote_crud(
        &mut self,
        repo: &RepoId,
        result: ramag_domain::error::Result<()>,
        success: String,
        clear_inputs: bool,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        self.busy_label = None;
        if !self.is_current_repo(repo) {
            cx.notify();
            return;
        }
        match result {
            Ok(()) => {
                info!(
                    operation = "git_remote_update",
                    repo_id = %repo,
                    "remote configuration updated"
                );
                if clear_inputs {
                    self.pending_clear_creation_inputs = true;
                }
                self.notify_success(success, cx);
                self.reload_remotes(cx);
            }
            Err(e) => {
                error!(
                    operation = "git_remote_update",
                    repo_id = %repo,
                    error = %e,
                    "remote configuration update failed"
                );
                self.error = Some(format!("远程操作失败：{e}"));
            }
        }
        cx.notify();
    }
}
