mod remote;
mod stash;
mod tag;

use gpui::Context;
use ramag_domain::entities::{BranchKind, LogOptions, MAX_COMMIT_MESSAGE_BYTES};
use tracing::{error, info};

use super::helpers::{BranchOp, FileOp, FileTabSource, HISTORY_PAGE_SIZE};
use super::vcs_view::VcsView;
use super::vcs_view_ops_history::parse_search_query;

impl VcsView {
    pub(in crate::views) fn run_branch_op(&mut self, op: BranchOp, cx: &mut Context<Self>) {
        if let Some(operation) = self.status.as_ref().and_then(|status| status.operation) {
            self.error = Some(format!(
                "{}仍在进行中：请先在顶部横幅选择继续或中止，再操作分支",
                super::helpers::operation_label(operation)
            ));
            cx.notify();
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        let label = match &op {
            BranchOp::Checkout(_) => "切换分支中…",
            BranchOp::Create(_, _) => "创建分支中…",
            BranchOp::Delete(_, _) => "删除分支中…",
            BranchOp::Merge(_) => "合并中…",
            BranchOp::Rebase(_) => "Rebase 中…",
        };
        if !self.begin_op(label, cx) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let result = match &op {
                BranchOp::Checkout(name) => driver.checkout(&repo, name).await,
                BranchOp::Create(name, base) => {
                    // 创建后立即切换；切换失败必须上报。
                    match driver.create_branch(&repo, name, base.as_deref()).await {
                        Ok(()) => driver.checkout(&repo, name).await.map_err(|e| {
                            ramag_domain::error::DomainError::Other(format!(
                                "分支「{name}」已创建，但切换失败：{e}"
                            ))
                        }),
                        Err(e) => Err(e),
                    }
                }
                BranchOp::Delete(name, force) => driver.delete_branch(&repo, name, *force).await,
                // --no-ff 强制创建 merge commit。
                BranchOp::Merge(name) => driver.merge(&repo, name, true, false, None).await,
                BranchOp::Rebase(name) => driver.rebase(&repo, name).await,
            };
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                &repo,
                "workspace status",
            );
            let new_local = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.list_branches(&repo, BranchKind::Local).await,
                &repo,
                "local branches",
            );
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                if let Some(branches) = new_local {
                    this.local_branches = branches;
                }
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                let paused = this.status.as_ref().and_then(|s| s.operation);
                match (&result, paused) {
                    (_, Some(operation)) => {
                        info!(
                            operation = "git_branch_operation",
                            repo_id = %repo,
                            paused_operation = ?operation,
                            branch_operation = ?op,
                            "branch operation paused"
                        );
                        this.handle_operation_paused(operation, cx);
                    }
                    (Err(e), None) => {
                        error!(
                            operation = "git_branch_operation",
                            repo_id = %repo,
                            branch_operation = ?op,
                            error = %e,
                            "branch operation failed"
                        );
                        this.error = Some(format!("分支操作失败：{e}"));
                    }
                    (Ok(_), None) => {
                        if matches!(op, BranchOp::Create(_, _)) {
                            this.pending_clear_creation_inputs = true;
                        }
                        let done_msg = match &op {
                            BranchOp::Checkout(n) => format!("已切换到 {n}"),
                            BranchOp::Create(n, _) => format!("已创建并切换到 {n}"),
                            BranchOp::Delete(n, _) => format!("已删除分支 {n}"),
                            BranchOp::Merge(n) => format!("已合并 {n}"),
                            BranchOp::Rebase(n) => format!("已 rebase 到 {n}"),
                        };
                        this.notify_success(done_msg, cx);
                    }
                }
                if (result.is_ok() || paused.is_some())
                    && matches!(
                        op,
                        BranchOp::Checkout(_)
                            | BranchOp::Merge(_)
                            | BranchOp::Rebase(_)
                            | BranchOp::Create(_, _)
                    )
                {
                    // HEAD 已变，缓存全部失效。
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// HEAD 变化后清理缓存并刷新。
    pub(in crate::views) fn refresh_after_head_change(&mut self, cx: &mut Context<Self>) {
        if self.compare.is_some() {
            self.clear_compare_state();
        }
        for tab in &mut self.file_tabs {
            tab.cached_diff = None;
            tab.cached_diff_syntax = None;
            tab.cached_content = None;
        }
        self.current_diff = None;
        self.current_diff_syntax = None;
        self.current_file_content = None;
        self.commit_file_diff = None;
        self.blame_lines = std::rc::Rc::new(Vec::new());

        self.refresh_current_files_view(cx);
        // 同步变更标签。
        self.sync_changes_tabs_with_status(cx);

        if let Some(idx) = self.active_file_tab_idx
            && let Some(tab) = self.file_tabs.get(idx).cloned()
        {
            match tab.source {
                // Changes 来源已重新加载。
                FileTabSource::Changes(_) => {}
                FileTabSource::ProjectFiles => {
                    self.select_pf_file(tab.path, cx);
                }
                FileTabSource::Commit { commit_id, .. } => {
                    self.select_commit_file(tab.path, commit_id, cx);
                }
                FileTabSource::Compare { .. } => {}
            }
        }
    }

    /// 启用 amend 时加载 HEAD 提交消息。
    pub(in crate::views) fn toggle_commit_amend(&mut self, cx: &mut Context<Self>) {
        if !self.commit_amend
            && self
                .status
                .as_ref()
                .and_then(|status| status.head_commit.as_ref())
                .is_none()
        {
            self.error = Some("当前仓库还没有 commit，无法 Amend；请先创建首次提交".into());
            cx.notify();
            return;
        }
        self.commit_amend = !self.commit_amend;
        cx.notify();
        if !self.commit_amend {
            return;
        }
        let input_empty = self.commit_input.read(cx).value().trim().is_empty();
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        if !input_empty {
            return;
        }
        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let head_msg = driver
                .commit_details(&repo, "HEAD")
                .await
                .map(|commit| commit.message_full());
            if let Err(error) = &head_msg {
                error!(
                    operation = "git_commit_amend_message",
                    repo_id = %repo,
                    error = %error,
                    "load amend message failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || !this.commit_amend {
                    return;
                }
                match head_msg {
                    // 用户已输入时不覆盖。
                    Ok(msg) if this.commit_input.read(cx).value().trim().is_empty() => {
                        this.pending_commit_text = Some(msg.into());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.error = Some(format!("加载上次提交消息失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::views) fn handle_create_branch(&mut self, cx: &mut Context<Self>) {
        if self
            .status
            .as_ref()
            .and_then(|status| status.head_commit.as_ref())
            .is_none()
        {
            self.error = Some("首次 commit 前不能从 HEAD 创建分支；请先完成首次提交".into());
            cx.notify();
            return;
        }
        let name = self.create_branch_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some("分支名不能为空".into());
            cx.notify();
            return;
        }
        let base = self.create_branch_base.take();
        self.run_branch_op(BranchOp::Create(name, base), cx);
    }

    pub(in crate::views) fn set_create_branch_base(
        &mut self,
        base: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.create_branch_base = base;
        cx.notify();
    }

    /// 加载提交历史页。
    pub(in crate::views) fn load_history_page(&mut self, skip: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        // unborn HEAD 直接显示空态，避免启动必然失败的 git log。
        if self
            .status
            .as_ref()
            .is_some_and(|status| status.head_commit.is_none())
        {
            self.history_request_seq = self.history_request_seq.wrapping_add(1);
            if skip == 0 {
                self.set_history_commits(Vec::new());
            }
            self.history_has_more = false;
            self.loading_history = false;
            cx.notify();
            return;
        }
        // 加载更多时避免重复拉页；刷新、切仓或换搜索仍需发起新请求。
        if skip > 0 && self.loading_history {
            return;
        }
        self.loading_history = true;
        // 请求代际防止旧回包乱序覆盖新结果。
        self.history_request_seq = self.history_request_seq.wrapping_add(1);
        let request_seq = self.history_request_seq;
        cx.notify();

        let driver = self.driver.clone();
        // @xxx 搜索作者，7d/1m 搜索时间，其余搜索提交说明。
        let raw_search = self
            .history_search_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let (grep, author, since) = parse_search_query(&raw_search);
        let history_start = self
            .history_ref_filter
            .as_ref()
            .map_or_else(|| "HEAD".to_owned(), |filter| filter.revision.clone());
        let opts = LogOptions {
            // 未选择分支或 tag 时从 HEAD 开始；选择引用后从完整 ref 开始。
            start: Some(history_start),
            skip,
            limit: Some(HISTORY_PAGE_SIZE),
            path_filter: self.history_path_filter.clone(),
            grep,
            author,
            since,
            ..Default::default()
        };
        cx.spawn(async move |this, cx| {
            let result = driver.log(&repo, opts).await;
            if let Err(error) = &result {
                error!(
                    operation = "git_history_load",
                    repo_id = %repo,
                    skip,
                    error = %error,
                    "load history failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.history_request_seq != request_seq {
                    cx.notify();
                    return;
                }
                this.loading_history = false;
                match result {
                    Ok(commits) => {
                        let got = commits.len();
                        let limit_reached = if skip == 0 {
                            this.set_history_commits(commits)
                        } else {
                            this.append_history_commits(commits)
                        };
                        this.history_has_more = got >= HISTORY_PAGE_SIZE && !limit_reached;
                    }
                    Err(e) => {
                        this.error = Some(format!("加载历史失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::views) fn run_commit(&mut self, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("创建普通提交", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let raw_message = self.commit_input.read(cx).value();
        if raw_message.len() > MAX_COMMIT_MESSAGE_BYTES {
            self.error = Some(format!(
                "commit message 超过 {} MiB 上限，请缩短后重试",
                MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
            ));
            cx.notify();
            return;
        }
        let message = raw_message.trim().to_string();
        if self.commit_amend
            && self
                .status
                .as_ref()
                .and_then(|status| status.head_commit.as_ref())
                .is_none()
        {
            self.error = Some("当前仓库还没有 commit，无法 Amend".into());
            cx.notify();
            return;
        }
        if message.is_empty() && !self.commit_amend {
            self.error = Some("commit message 不能为空".into());
            cx.notify();
            return;
        }
        let amend = self.commit_amend;
        let sign = self.commit_sign;
        let driver = self.driver.clone();
        if !self.begin_op("提交中…", cx) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let result = driver.commit(&repo, &message, amend, sign).await;
            if let Err(error) = &result {
                error!(
                    operation = "git_commit",
                    repo_id = %repo,
                    amend,
                    sign,
                    message_bytes = message.len(),
                    error = %error,
                    "commit failed"
                );
            }
            let new_status = if result.is_ok() {
                crate::views::vcs_view_ops_sync::best_effort_refresh(
                    driver.status(&repo).await,
                    &repo,
                    "workspace status",
                )
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(commit_id) => {
                        info!(
                            operation = "git_commit",
                            repo_id = %repo,
                            commit = %commit_id,
                            amend,
                            sign,
                            "commit completed"
                        );
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        this.commit_amend = false;
                        // 提交后清空消息和草稿，并使在途草稿写入失效。
                        this.pending_commit_text = Some(gpui::SharedString::default());
                        this.commit_draft_gen
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        this.commit_draft_error = None;
                        if let Some(path) = this.repo.as_ref().map(|r| r.path.clone()) {
                            let storage = this.storage.clone();
                            cx.background_executor()
                                .spawn(async move {
                                    let key =
                                        super::vcs_view_ops_repo::commit_draft_pref_key(&path);
                                    if let Err(e) = storage.set_preference(&key, "").await {
                                        tracing::warn!(
                                            operation = "git_commit_draft_clear",
                                            error = %e,
                                            "clear commit draft failed"
                                        );
                                    }
                                })
                                .detach();
                        }
                        this.sync_changes_tabs_with_status(cx);
                        // 已加载或打开 History 时，立即刷新新提交。
                        if this.history_pane_visible || !this.history_commits.is_empty() {
                            this.load_history_page(0, cx);
                        }
                        let short: String = commit_id.0.chars().take(7).collect();
                        this.notify_success(format!("已提交 {short}"), cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("提交失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 执行暂存、取消暂存或丢弃。
    pub(in crate::views) fn run_file_op(
        &mut self,
        op: FileOp,
        paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let driver = self.driver.clone();
        let label = match op {
            FileOp::Stage => "暂存中…",
            FileOp::Unstage => "取消暂存中…",
            FileOp::Discard => "丢弃改动中…",
        };
        if !self.begin_op(label, cx) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let result = match op {
                FileOp::Stage => driver.stage(&repo, &paths).await,
                FileOp::Unstage => driver.unstage(&repo, &paths).await,
                FileOp::Discard => driver.discard(&repo, &paths).await,
            };
            if let Err(error) = &result {
                error!(
                    operation = "git_file_operation",
                    repo_id = %repo,
                    file_operation = ?op,
                    path_count = paths.len(),
                    error = %error,
                    "file operation failed"
                );
            }
            let new_status = if result.is_ok() {
                crate::views::vcs_view_ops_sync::best_effort_refresh(
                    driver.status(&repo).await,
                    &repo,
                    "workspace status",
                )
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(_) => {
                        info!(
                            operation = "git_file_operation",
                            repo_id = %repo,
                            file_operation = ?op,
                            path_count = paths.len(),
                            "file operation completed"
                        );
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        // 文件组别迁移后同步标签。
                        this.sync_changes_tabs_with_status(cx);
                        if matches!(op, FileOp::Discard) {
                            let target = if paths.len() == 1 {
                                paths[0].clone()
                            } else {
                                format!("{} 个文件", paths.len())
                            };
                            this.notify_success(format!("已丢弃 {target} 的改动"), cx);
                        }
                    }
                    Err(e) => {
                        let action = match op {
                            FileOp::Stage => "暂存",
                            FileOp::Unstage => "取消暂存",
                            FileOp::Discard => "丢弃改动",
                        };
                        this.error = Some(format!("{action}失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
