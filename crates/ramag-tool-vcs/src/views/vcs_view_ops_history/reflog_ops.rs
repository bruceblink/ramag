//! VcsView reflog 相关 ops：toggle 视图 / 加载 reflog / checkout 到 reflog 条目

use gpui::Context;
use tracing::{error, info};

use super::super::vcs_view::VcsView;

impl VcsView {
    /// 切换 reflog / commit 视图
    pub(crate) fn toggle_reflog(&mut self, cx: &mut Context<Self>) {
        self.showing_reflog = !self.showing_reflog;
        if self.showing_reflog {
            self.load_reflog(cx);
        } else {
            self.reflog_request_seq = self.reflog_request_seq.wrapping_add(1);
            self.loading_reflog = false;
        }
        cx.notify();
    }

    /// 异步拉取 reflog（默认 HEAD，最多 200 条）
    pub(crate) fn load_reflog(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_reflog = true;
        self.reflog_request_seq = self.reflog_request_seq.wrapping_add(1);
        let request_seq = self.reflog_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_reflog(&repo, None, Some(200)).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.reflog_request_seq != request_seq {
                    return;
                }
                this.loading_reflog = false;
                match result {
                    Ok(entries) => {
                        this.reflog_entries = std::rc::Rc::new(entries);
                        this.reflog_rows_cache.get_mut().take();
                    }
                    Err(e) => {
                        error!(
                            operation = "vcs_reflog_load",
                            repo_id = %repo,
                            error = %e,
                            "load reflog failed"
                        );
                        this.error = Some(format!("加载 reflog 失败：{e}"));
                        this.showing_reflog = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// reflog 条目点击 → checkout 到该 commit（detached HEAD；checkout 后切回 commit 历史）
    pub(crate) fn checkout_reflog_entry(&mut self, commit: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("Checkout 历史版本", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("切换到历史版本中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.checkout(&repo, &commit).await;
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                &repo,
                "workspace status",
            );
            // detached HEAD 后分支的 is_head 标记会变，同步刷新
            let new_local = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver
                    .list_branches(&repo, ramag_domain::entities::BranchKind::Local)
                    .await,
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
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                if let Some(branches) = new_local {
                    this.local_branches = branches;
                }
                if let Err(e) = result {
                    error!(
                        operation = "vcs_reflog_checkout",
                        repo_id = %repo,
                        commit_id = %commit,
                        error = %e,
                        "reflog checkout failed"
                    );
                    this.error = Some(format!("Checkout 到 {commit} 失败：{e}"));
                } else {
                    info!(
                        operation = "vcs_reflog_checkout",
                        repo_id = %repo,
                        commit_id = %commit,
                        status = "completed",
                        "reflog checkout completed"
                    );
                    this.showing_reflog = false;
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    let short: String = commit.chars().take(7).collect();
                    this.notify_success(format!("已 checkout 到 {short}（detached HEAD）"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
