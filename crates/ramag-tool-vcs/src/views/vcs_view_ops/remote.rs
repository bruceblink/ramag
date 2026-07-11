//! VcsView Remote 异步操作：fetch / pull / push（含 force-with-lease）

use gpui::Context;
use ramag_domain::entities::BranchKind;
use tracing::{error, info};

use super::super::helpers::RemoteOp;
use super::super::vcs_view::VcsView;

impl VcsView {
    /// fetch=`--all --prune`；push 无 upstream 自动 -u；pull 无 upstream 引导先 push
    pub(in crate::views) fn run_remote_op(&mut self, op: RemoteOp, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let Some(local_branch) = self.status.as_ref().and_then(|s| s.head_branch.clone()) else {
            self.error = Some("当前为 detached HEAD，无法 push/pull".into());
            cx.notify();
            return;
        };
        // 从 local_branches 找当前 head 的 upstream（"origin/main"）
        let upstream = self
            .local_branches
            .iter()
            .find(|b| b.is_head)
            .and_then(|b| b.upstream.clone());
        let (remote_name, remote_branch) = match upstream.as_deref().and_then(|u| u.split_once('/'))
        {
            Some((r, b)) => (r.to_string(), b.to_string()),
            None => ("origin".to_string(), local_branch.clone()),
        };
        let need_set_upstream = upstream.is_none();
        // PushForce → 走 --force-with-lease；其他 op 忽略
        let this_force_lease = matches!(op, RemoteOp::PushForce);
        // pull 模式下若没有 upstream 直接报错引导（避免提示「fatal: no tracking info」）
        if matches!(op, RemoteOp::Pull) && need_set_upstream {
            self.error =
                Some("当前分支没有上游分支：先点 Push（会自动设置 upstream）再 Pull".into());
            cx.notify();
            return;
        }
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

        cx.spawn(async move |this, cx| {
            let result = match op {
                // 空 remote 让 driver 拉所有 remote
                RemoteOp::Fetch => driver.fetch(&repo, "").await,
                RemoteOp::Pull => {
                    driver
                        .pull(&repo, &remote_name, &remote_branch, false)
                        .await
                }
                RemoteOp::Push | RemoteOp::PushForce => {
                    driver
                        .push(
                            &repo,
                            &remote_name,
                            &local_branch,
                            need_set_upstream,
                            this_force_lease,
                        )
                        .await
                }
            };
            // 不论成功失败都刷新一次 status（pull 后 ahead/behind 必变）；
            // remote 分支同刷：fetch/pull 更新远端 refs，push -u 会新建 origin/<branch>
            let new_status = driver.status(&repo).await.ok();
            let local = driver.list_branches(&repo, BranchKind::Local).await.ok();
            let remote_b = driver.list_branches(&repo, BranchKind::Remote).await.ok();
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
                if let Some(b) = local {
                    this.local_branches = b;
                }
                if let Some(b) = remote_b {
                    this.remote_branches = b;
                }
                match result {
                    Err(e) => {
                        error!(error = %e, ?op, "vcs: remote op failed");
                        this.error = Some(format!("{op_label} 失败：{e}"));
                    }
                    Ok(_) => {
                        info!(?op, "vcs: remote op done");
                        // fetch 后 behind 增加 = 远端有新 commit 被发现
                        let post_behind =
                            this.status.as_ref().and_then(|s| s.behind).unwrap_or(0);
                        let msg = match op {
                            RemoteOp::Fetch if post_behind > pre_behind => {
                                format!("Fetch 完成：发现 {} 个新 commit", post_behind - pre_behind)
                            }
                            RemoteOp::Fetch => "Fetch 完成：已是最新".to_string(),
                            RemoteOp::Pull if pre_behind == 0 => {
                                format!("已是最新（{remote_name}/{remote_branch} 无新 commit）")
                            }
                            RemoteOp::Pull => format!(
                                "Pull 完成：合入 {pre_behind} 个 commit（{remote_name}/{remote_branch}）"
                            ),
                            RemoteOp::Push if need_set_upstream => {
                                format!("Push 成功，已设置 upstream {remote_name}/{local_branch}")
                            }
                            RemoteOp::Push if pre_ahead == 0 => {
                                format!("已是最新（{remote_name}/{local_branch} 无待推送 commit）")
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
}
