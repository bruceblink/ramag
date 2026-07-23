//! 版本管理异步操作。

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
                    // 等价 `git checkout -b`：创建后立即 checkout；
                    // checkout 失败必须如实上报，否则界面谎称"已切换"
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
                // --no-ff 强制建 merge commit；冲突时仓库进入 Merge 状态
                BranchOp::Merge(name) => driver.merge(&repo, name, true, false, None).await,
                BranchOp::Rebase(name) => driver.rebase(&repo, name).await,
            };
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                "workspace status",
            );
            let new_local = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.list_branches(&repo, BranchKind::Local).await,
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
                        info!(?operation, ?op, "branch operation paused");
                        this.handle_operation_paused(operation, cx);
                    }
                    (Err(e), None) => {
                        error!(error = %e, ?op, "branch operation failed");
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
                    // HEAD 变了，缓存全失效
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// HEAD 变化（checkout / merge / rebase / 建分支 / pull）：清缓存 + 重拉，避免显示旧分支内容
    pub(in crate::views) fn refresh_after_head_change(&mut self, cx: &mut Context<Self>) {
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
        // Changes tabs 对齐新 status（关已无变更的 / 重定向组别），active 是 Changes 时由它重拉
        self.sync_changes_tabs_with_status(cx);

        if let Some(idx) = self.active_file_tab_idx
            && let Some(tab) = self.file_tabs.get(idx).cloned()
        {
            match tab.source {
                // Changes 来源已由 sync_changes_tabs_with_status 重拉
                FileTabSource::Changes(_) => {}
                FileTabSource::ProjectFiles => {
                    self.select_pf_file(tab.path, cx);
                }
                FileTabSource::Commit { commit_id, .. } => {
                    self.select_commit_file(tab.path, commit_id, cx);
                }
            }
        }
    }

    /// 切换 amend：勾上且 message 为空时，异步拉 HEAD 的 message 填入输入框（IDEA 同款），
    /// 方便在原文基础上改；取消勾选不动已输入内容
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
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || !this.commit_amend {
                    return;
                }
                match head_msg {
                    // 异步期间用户已输入内容则不覆盖
                    Ok(msg) if this.commit_input.read(cx).value().trim().is_empty() => {
                        this.pending_commit_text = Some(msg.into());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(error = %error, "load amend message failed");
                        this.error = Some(format!("加载上次提交消息失败：{error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// base=None 时从当前 HEAD 建
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

    /// skip=0 覆盖刷新，其他值 append
    pub(in crate::views) fn load_history_page(&mut self, skip: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        // status 已确认 unborn HEAD 时直接给空态，避免为必然失败的 `git log HEAD` 启进程。
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
        // skip>0 是 load-more：正在加载时跳过避免重复拉同一页；
        // skip=0 是刷新/切仓/换搜索，即使有在途请求也要发起（否则切仓后新仓库 history 会因早退而不加载）
        if skip > 0 && self.loading_history {
            return;
        }
        self.loading_history = true;
        // 请求代际：换搜索词/刷新后才返回的旧回包据此丢弃，避免乱序覆盖新结果
        self.history_request_seq = self.history_request_seq.wrapping_add(1);
        let request_seq = self.history_request_seq;
        cx.notify();

        let driver = self.driver.clone();
        // `@xxx`→author，`7d`/`1m`→since，其余→message grep
        let raw_search = self
            .history_search_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let (grep, author, since) = parse_search_query(&raw_search);
        let opts = LogOptions {
            // UI 已由 status 排除 unborn HEAD；显式 HEAD 让 infra 省掉额外 rev-parse 探测。
            start: Some("HEAD".into()),
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
                        error!(error = %e, "load history failed");
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
            let new_status = if result.is_ok() {
                crate::views::vcs_view_ops_sync::best_effort_refresh(
                    driver.status(&repo).await,
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
                        info!(commit = %commit_id, "commit completed");
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        this.commit_amend = false;
                        // 提交成功：清空 message（避免下次误用同一条），已提交文件的 tabs 对齐；
                        // 持久化的草稿一并清除（作废在途防抖写）
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
                                        tracing::warn!(error = %e, "clear commit draft failed");
                                    }
                                })
                                .detach();
                        }
                        this.sync_changes_tabs_with_status(cx);
                        // history 已加载过 / 面板开着 → 立即把新 commit 刷到列表顶部
                        if this.history_pane_visible || !this.history_commits.is_empty() {
                            this.load_history_page(0, cx);
                        }
                        let short: String = commit_id.0.chars().take(7).collect();
                        this.notify_success(format!("已提交 {short}"), cx);
                    }
                    Err(e) => {
                        error!(error = %e, "commit failed");
                        this.error = Some(format!("提交失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// stage / unstage / discard，支持多文件批量（「全部 Stage」一次任务搞定，只刷一次 status）
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
            let new_status = if result.is_ok() {
                crate::views::vcs_view_ops_sync::best_effort_refresh(
                    driver.status(&repo).await,
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
                        if let Some(s) = new_status {
                            this.status = Some(s);
                        }
                        // 组别迁移（如 stage 后 Unstaged → Staged）跟着对齐
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
                        error!(error = %e, ?op, ?paths, "file operation failed");
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
