//! VcsView 历史变更（破坏性 / HEAD 移动）：Reset / Revert / 切换分支前的 stash / discard

use gpui::Context;
use ramag_domain::entities::{BranchKind, ResetKind};
use tracing::{error, info};

use super::super::helpers::{BranchOp, reset_kind_label};
use super::super::vcs_view::VcsView;

impl VcsView {
    /// Revert：生成一个反向 commit 撤销指定 commit
    pub(crate) fn run_revert(&mut self, commit_id: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("执行 Revert", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Revert 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.revert(&repo, &commit_id).await;
            let new_status = crate::views::vcs_view_ops_sync::best_effort_refresh(
                driver.status(&repo).await,
                "workspace status",
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
                let paused = this.status.as_ref().and_then(|s| s.operation);
                if let Some(operation) = paused {
                    info!(%commit_id, ?operation, "vcs: revert paused");
                    this.handle_operation_paused(operation, cx);
                    this.refresh_after_head_change(cx);
                } else if let Err(e) = result {
                    error!(error = %e, %commit_id, "vcs: revert failed");
                    this.error = Some(format!("Revert 失败：{e}"));
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
        if !self.ensure_no_operation("执行 Reset", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Reset 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.reset(&repo, &target, kind).await;
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
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                if let Some(branches) = new_local {
                    this.local_branches = branches;
                }
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
        if !self.ensure_no_operation("切换版本", cx) {
            return;
        }
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
            let (stash_saved, final_result) = match stash_result {
                Ok(()) => (true, driver.checkout(&repo, &target).await),
                Err(e) => (false, Err(e)),
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
                        error!(error = %e, target = %target_for_log, stash_saved, "vcs: stash+checkout failed");
                        this.error = Some(if stash_saved {
                            format!(
                                "切换失败，但改动已安全保存在最新一条 Stash 中，可手动恢复：{e}"
                            )
                        } else {
                            format!("Stash 失败，未切换且原工作区改动仍保留：{e}")
                        });
                        this.reload_stashes(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 工作区 dirty 时切换分支：临时 stash → checkout → 成功后 drop 临时 stash。
    /// 仍兑现“成功后永久丢弃”，但 checkout 失败时保留恢复点，避免先丢改动再切换失败。
    pub(crate) fn run_checkout_with_discard(&mut self, target: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("切换版本", cx) {
            return;
        }
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
            let marker = format!(
                "ramag-discard-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            );
            let message = format!("{marker} before checkout to {target_for_log}");
            let stash_result = driver.stash_save(&repo, Some(&message), true).await;
            let (stash_saved, checkout_result) = match stash_result {
                Ok(()) => (true, driver.checkout(&repo, &target).await),
                Err(e) => (false, Err(e)),
            };
            // 仅 checkout 成功后永久删除；按唯一 marker 重新定位，避免外部进程新增 stash
            // 导致 index 0 指向别人的条目。
            let drop_result = if checkout_result.is_ok() {
                let temporary_idx = driver.list_stashes(&repo).await.map(|stashes| {
                    stashes
                        .iter()
                        .find(|stash| stash.message.contains(&marker))
                        .map(|stash| stash.id.0)
                });
                Some(match temporary_idx {
                    Ok(Some(idx)) => driver.stash_drop(&repo, idx).await,
                    Ok(None) => Err(ramag_domain::error::DomainError::Other(
                        "未找到 Ramag 创建的临时 stash，已保留全部 stash 以避免误删".into(),
                    )),
                    Err(error) => {
                        tracing::error!(error = %error, "vcs: list temporary stash failed");
                        Err(ramag_domain::error::DomainError::Other(format!(
                            "无法读取 Stash 列表，为避免误删已保留临时备份：{error}"
                        )))
                    }
                })
            } else {
                None
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
                match checkout_result {
                    Ok(()) => {
                        info!(target = %target_for_log, "vcs: discard + checkout done");
                        this.load_history_page(0, cx);
                        this.refresh_after_head_change(cx);
                        this.reload_stashes(cx);
                        match drop_result {
                            Some(Ok(())) => this.notify_success(
                                format!("已丢弃工作区改动并切换到 {target_for_log}"),
                                cx,
                            ),
                            Some(Err(e)) => {
                                error!(error = %e, target = %target_for_log, "vcs: checkout done but temporary stash drop failed");
                                this.error = Some(format!(
                                    "已切换到 {target_for_log}，但临时备份删除失败（改动仍可在 Stash 恢复）：{e}"
                                ));
                            }
                            None => {}
                        }
                    }
                    Err(e) => {
                        error!(error = %e, target = %target_for_log, stash_saved, "vcs: guarded discard checkout failed");
                        this.error = Some(if stash_saved {
                            format!(
                                "切换失败，未丢失改动：已保存在最新一条 Stash 中，可手动恢复。原因：{e}"
                            )
                        } else {
                            format!("创建临时保护失败，未切换也未丢弃改动：{e}")
                        });
                        this.reload_stashes(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
