//! VcsView Remote 异步操作：fetch / pull / push（含 force-with-lease）

use gpui::Context;
use ramag_domain::entities::RepoId;
use tracing::{error, info};

use super::super::helpers::{RemoteOp, default_remote_name, is_current_arc_slot};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// fetch=`--all --prune`；push 无 upstream 自动 -u；pull 无 upstream 引导先 push
    pub(in crate::views) fn run_remote_op(&mut self, op: RemoteOp, cx: &mut Context<Self>) {
        self.run_remote_op_to(op, None, cx);
    }

    /// `selected_remote` 仅用于首次 Push 的显式选择；已有 upstream 时仍严格跟随 upstream。
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
        // Fetch 与 HEAD 无关；detached HEAD 下仍应允许更新远端引用。
        let local_branch = local_branch.unwrap_or_default();
        // 从 local_branches 找当前 head 的 upstream（"origin/main"）
        let upstream = self
            .local_branches
            .iter()
            .find(|b| b.is_head)
            .and_then(|b| b.upstream.clone());
        let need_set_upstream = upstream.is_none();
        // pull 模式下若没有 upstream 直接报错引导（避免提示「fatal: no tracking info」）
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
            // Fetch 拉所有 remote，不使用这里的名字。
            None => (String::new(), local_branch.clone()),
        };
        // PushForce → 走 --force-with-lease；其他 op 忽略
        let this_force_lease = matches!(op, RemoteOp::PushForce);
        let driver = self.driver.clone();
        // 操作前的 ahead/behind：完成后据此区分「真的推/拉了 N 个」与「本来就是最新」
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
        // 进度槽 + 取消位：streaming 变体持续写进度、监听取消；存入 self 供工具栏展示 / 取消按钮
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.remote_op_cancel = Some(cancel.clone());
        self.remote_op_progress = Some(progress.clone());
        // 进度轮询：每 120ms notify 让工具栏刷新最新进度行，操作结束（槽被清）即退出
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
                // 空 remote 让 driver 拉所有 remote
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
            // 不论成功失败都刷新一次 status（pull 后 ahead/behind 必变）；
            // remote 分支同刷：fetch/pull 更新远端 refs，push -u 会新建 origin/<branch>
            let (new_status, branches) = futures::future::join(
                driver.status(&repo),
                driver.list_all_branches(&repo),
            )
            .await;
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                new_status,
                "workspace status",
            );
            let branches = crate::views::vcs_view_ops_sync::best_effort_refresh(
                branches,
                "branches",
            );
            let _ = this.update(cx, |this, cx| {
                // 只允许发起该任务的操作收尾，避免迟到回调清掉后续操作的状态槽。
                if !is_current_arc_slot(this.remote_op_cancel.as_ref(), &cancel) {
                    return;
                }
                this.busy = false;
                this.busy_label = None;
                // 清进度 / 取消槽（也让轮询任务下一拍退出）
                let was_cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
                this.remote_op_cancel = None;
                this.remote_op_progress = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                // 用户主动取消：不当作失败刷错误横幅，给中性提示
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
                        info!(?operation, "pull paused");
                        this.handle_operation_paused(operation, cx);
                        this.refresh_after_head_change(cx);
                    }
                    (Err(e), None) => {
                        error!(error = %e, ?op, "remote operation failed");
                        this.error = Some(format!("{op_label} 失败：{e}"));
                    }
                    (Ok(_), None) => {
                        info!(?op, "remote operation completed");
                        // fetch 后 behind 增加 = 远端有新 commit 被发现
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
                        // Pull 可能带来新 commit：HEAD 内容变了，缓存全失效 + history 刷新
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

    /// 取消进行中的远端操作：置取消位，infra watcher kill git 子进程；
    /// 收尾在异步完成回调里统一清槽 + 中性提示
    pub(in crate::views) fn cancel_remote_op(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.remote_op_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.busy_label = Some("取消中…");
            cx.notify();
        }
    }

    /// 远端操作进行中的最新进度行（工具栏展示用）；无操作时 None
    pub(in crate::views) fn remote_op_progress_line(&self) -> Option<String> {
        let slot = self.remote_op_progress.as_ref()?;
        let text = match slot.try_lock() {
            Ok(text) => text.clone(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(error)) => {
                tracing::warn!("remote progress lock poisoned");
                error.into_inner().clone()
            }
        };
        if text.is_empty() { None } else { Some(text) }
    }

    /// 「添加远程」按钮：读双输入 → 校验非空 → add_remote_op
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

    /// git remote add；成功后清空创建输入并刷新列表
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

    /// git remote set-url（改 fetch URL）
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

    /// 远程 CRUD 统一收尾：复位忙碌 → 归属校验 → 成功刷新列表+toast / 失败错误横幅。
    /// clear_inputs=true 时（仅添加）经 pending 标志清空创建输入
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
