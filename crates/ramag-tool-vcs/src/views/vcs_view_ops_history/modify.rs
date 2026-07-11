//! VcsView 历史变更（破坏性 / HEAD 移动）：Reset / Revert / 切换分支前的 stash / discard

use gpui::Context;
use ramag_domain::entities::{BranchKind, ResetKind};
use tracing::{error, info};

use super::super::helpers::{BranchOp, reset_kind_label};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// Revert：生成一个反向 commit 撤销指定 commit
    pub(crate) fn run_revert(&mut self, commit_id: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Revert 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.revert(&repo, &commit_id).await;
            let new_status = driver.status(&repo).await.ok();
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
                if let Err(e) = result {
                    error!(error = %e, %commit_id, "vcs: revert failed");
                    this.error = Some(format!("Revert 失败：{e}（如有冲突请到工作区处理）"));
                } else {
                    info!(%commit_id, "vcs: revert done");
                    // HEAD 推进一个 revert commit，刷新 history 第一页
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    let short: String = commit_id.chars().take(7).collect();
                    this.notify_success(format!("已 Revert {short}"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reset：移动 HEAD 到指定 commit（默认 mixed，hard 留弹框确认避免误操作）
    pub(crate) fn run_reset(&mut self, target: String, kind: ResetKind, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Reset 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.reset(&repo, &target, kind).await;
            let new_status = driver.status(&repo).await.ok();
            let new_local = driver
                .list_branches(&repo, BranchKind::Local)
                .await
                .unwrap_or_default();
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
                this.local_branches = new_local;
                if let Err(e) = result {
                    error!(error = %e, %target, ?kind, "vcs: reset failed");
                    this.error = Some(format!("Reset {} 失败：{e}", reset_kind_label(kind)));
                } else {
                    info!(%target, ?kind, "vcs: reset done");
                    // HEAD 移动了：history、暂存区、已打开 tabs 的 diff 缓存全部重拉
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    let short: String = target.chars().take(7).collect();
                    this.notify_success(
                        format!("已 Reset {} 到 {short}", reset_kind_label(kind)),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 工作区 dirty 切换分支：stash → checkout（stash 不自动 pop，用户在 Stash 面板手动 apply）
    pub(crate) fn run_checkout_with_stash(&mut self, target: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Stash 并切换中…", cx) {
            return;
        }
        let target_for_log = target.clone();
        cx.spawn(async move |this, cx| {
            let msg = format!("auto-stash before checkout to {target_for_log}");
            let stash_result = driver.stash_save(&repo, Some(&msg), true).await;
            let final_result = match stash_result {
                Ok(()) => driver.checkout(&repo, &target).await,
                Err(e) => Err(e),
            };
            let new_status = driver.status(&repo).await.ok();
            let new_local = driver
                .list_branches(&repo, BranchKind::Local)
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                this.local_branches = new_local;
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                match final_result {
                    Ok(()) => {
                        info!(target = %target_for_log, "vcs: stash + checkout done");
                        this.load_history_page(0, cx);
                        this.refresh_after_head_change(cx);
                        this.reload_stashes(cx);
                        this.notify_success(
                            format!("已 stash 工作区改动并切换到 {target_for_log}"),
                            cx,
                        );
                    }
                    Err(e) => {
                        error!(error = %e, target = %target_for_log, "vcs: stash+checkout failed");
                        this.error = Some(format!("Stash 后切换失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 工作区 dirty 时切换分支：丢弃改动 → checkout（不可逆，调用前已确认）。
    /// 用 `reset --hard HEAD` 同时清空暂存区与工作区的 tracked 改动——
    /// 之前的 `git checkout -- paths` 清不掉 index，已暂存内容会被带进新分支或阻塞切换。
    /// 未跟踪的新文件保留（不阻塞 checkout；与目标分支冲突的罕见情形按错误如实上报）
    pub(crate) fn run_checkout_with_discard(&mut self, target: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let has_dirty = self.status.as_ref().is_some_and(|s| {
            s.files
                .iter()
                .any(|f| f.staged.is_some() || f.unstaged.is_some())
        });
        if !has_dirty {
            self.run_branch_op(BranchOp::Checkout(target), cx);
            return;
        }
        let driver = self.driver.clone();
        if !self.begin_op("切换分支中…", cx) {
            return;
        }
        let target_for_log = target.clone();
        cx.spawn(async move |this, cx| {
            let discard_result = driver.reset(&repo, "HEAD", ResetKind::Hard).await;
            let final_result = match discard_result {
                Ok(()) => driver.checkout(&repo, &target).await,
                Err(e) => Err(e),
            };
            let new_status = driver.status(&repo).await.ok();
            let new_local = driver
                .list_branches(&repo, BranchKind::Local)
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                this.local_branches = new_local;
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                match final_result {
                    Ok(()) => {
                        info!(target = %target_for_log, "vcs: discard + checkout done");
                        this.load_history_page(0, cx);
                        this.refresh_after_head_change(cx);
                        this.notify_success(
                            format!("已丢弃工作区改动并切换到 {target_for_log}"),
                            cx,
                        );
                    }
                    Err(e) => {
                        error!(error = %e, target = %target_for_log, "vcs: discard+checkout failed");
                        this.error = Some(format!("丢弃后切换失败：{e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
