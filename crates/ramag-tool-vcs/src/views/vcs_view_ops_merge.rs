//! 合并 / cherry-pick / 冲突解决：cherry_pick / use ours/theirs / 已解决 / 进行中 op 的继续 / 中止

use gpui::Context;
use ramag_domain::entities::{BranchKind, RepoOperation};
use tracing::{error, info};

impl VcsView {
    /// 打开三方冲突编辑器：异步拉取 ours / theirs 内容并展示
    pub(super) fn open_conflict_editor(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        let path_clone = path.clone();
        self.conflict_editor_path = Some(path);
        self.conflict_content = None;
        self.loading_conflict = true;
        self.conflict_request_seq = self.conflict_request_seq.wrapping_add(1);
        let request_seq = self.conflict_request_seq;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = driver.get_conflict_content(&repo, &path_clone).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo)
                    || this.conflict_request_seq != request_seq
                    || this.conflict_editor_path.as_deref() != Some(path_clone.as_str())
                {
                    return;
                }
                this.loading_conflict = false;
                match result {
                    Ok(content) => this.conflict_content = Some(content),
                    Err(e) => {
                        error!(error = %e, path = %path_clone, "vcs: get conflict content failed");
                        this.error = Some(format!("加载冲突内容失败：{e}"));
                        this.conflict_editor_path = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

use super::helpers::{ConflictOp, OperationStep, operation_label, step_label};
use super::vcs_view::VcsView;

impl VcsView {
    /// Git 已进入可恢复的进行中状态：这是操作暂停，不是终态失败。
    pub(super) fn handle_operation_paused(
        &mut self,
        operation: RepoOperation,
        cx: &mut Context<Self>,
    ) {
        let conflicts = self
            .status
            .as_ref()
            .map(|s| s.files.iter().filter(|f| f.is_conflicted()).count())
            .unwrap_or(0);
        self.error = None;
        self.view_mode = super::helpers::ViewMode::Workspace;
        self.files_view_mode = super::helpers::FilesViewMode::Changes;
        self.sync_changes_tabs_with_status(cx);
        let message = if conflicts > 0 {
            format!(
                "{}已暂停：请先解决 {conflicts} 个冲突文件，再点「继续」",
                operation_label(operation)
            )
        } else {
            format!(
                "{}已暂停：确认当前改动后点「继续」，或点「中止」回滚",
                operation_label(operation)
            )
        };
        self.notify_warning(message, cx);
    }

    /// Cherry-pick 单个 commit 到当前 HEAD
    pub(super) fn run_cherry_pick(&mut self, commit_id: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("执行 Cherry-pick", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("Cherry-pick 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver.cherry_pick(&repo, &commit_id).await;
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
                let paused = this.status.as_ref().and_then(|s| s.operation);
                if let Some(operation) = paused {
                    info!(%commit_id, ?operation, "vcs: cherry-pick paused");
                    this.handle_operation_paused(operation, cx);
                    this.refresh_after_head_change(cx);
                } else if let Err(e) = result {
                    error!(error = %e, %commit_id, "vcs: cherry-pick failed");
                    this.error = Some(format!("Cherry-pick 失败：{e}"));
                } else {
                    info!(%commit_id, "vcs: cherry-pick done");
                    // HEAD 推进了一个新 commit
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    let short: String = commit_id.chars().take(7).collect();
                    this.notify_success(format!("已 cherry-pick {short}"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 冲突文件解决：Use Ours / Use Theirs / Mark Resolved
    pub(super) fn run_conflict_op(&mut self, op: ConflictOp, path: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("处理冲突中…", cx) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let paths = vec![path.clone()];
            let result = match op {
                ConflictOp::UseOurs => driver.use_ours(&repo, &paths).await,
                ConflictOp::UseTheirs => driver.use_theirs(&repo, &paths).await,
                ConflictOp::MarkResolved => driver.stage(&repo, &paths).await,
            };
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
                    error!(error = %e, ?op, %path, "vcs: conflict op failed");
                    this.error = Some(format!("冲突操作失败：{e}"));
                } else {
                    info!(?op, %path, "vcs: conflict op done");
                    // 该文件已解决：若三栏编辑器正开着它，关闭回 diff 视图
                    if this.conflict_editor_path.as_deref() == Some(path.as_str()) {
                        this.conflict_editor_path = None;
                        this.conflict_content = None;
                    }
                    // 文件离开冲突组（→ 已暂存）：tabs 跟着迁移
                    this.sync_changes_tabs_with_status(cx);
                    let labels = super::workspace_conflict::conflict_side_labels(
                        this.status.as_ref().and_then(|status| status.operation),
                    );
                    let what = match op {
                        ConflictOp::UseOurs => format!("已采纳「{}」版本", labels.0),
                        ConflictOp::UseTheirs => format!("已采纳「{}」版本", labels.1),
                        ConflictOp::MarkResolved => "已标记为已解决".into(),
                    };
                    this.notify_success(format!("{what}：{path}"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 进行中操作的 [继续 | 中止]：按 status.operation 派发到合适的 driver 方法
    pub(super) fn run_op_step(&mut self, step: OperationStep, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let Some(operation) = self.status.as_ref().and_then(|s| s.operation) else {
            self.error = Some("当前没有进行中的合并 / cherry-pick".into());
            cx.notify();
            return;
        };
        let driver = self.driver.clone();
        if !self.begin_op("处理中…", cx) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let result = match (operation, step) {
                (RepoOperation::Merge, OperationStep::Continue) => {
                    driver.merge_continue(&repo).await
                }
                (RepoOperation::Merge, OperationStep::Abort) => driver.merge_abort(&repo).await,
                (RepoOperation::CherryPick, OperationStep::Continue) => {
                    driver.cherry_pick_continue(&repo).await
                }
                (RepoOperation::CherryPick, OperationStep::Abort) => {
                    driver.cherry_pick_abort(&repo).await
                }
                (RepoOperation::Rebase, OperationStep::Continue) => {
                    driver.rebase_continue(&repo).await
                }
                (RepoOperation::Rebase, OperationStep::Skip) => driver.rebase_skip(&repo).await,
                (RepoOperation::Rebase, OperationStep::Abort) => driver.rebase_abort(&repo).await,
                (RepoOperation::Revert, OperationStep::Continue) => {
                    driver.revert_continue(&repo).await
                }
                (RepoOperation::Revert, OperationStep::Abort) => driver.revert_abort(&repo).await,
                // Merge / CherryPick / Revert 不支持 Skip（横幅按钮已按 operation 置灰）
                _ => Err(ramag_domain::error::DomainError::NotImplemented(format!(
                    "{}·{}",
                    operation_label(operation),
                    step_label(step)
                ))),
            };
            // 操作后刷新 status + branches（merge 完会切回干净状态，分支 ahead/behind 也变了）
            let new_status = driver.status(&repo).await.ok();
            let new_local = driver.list_branches(&repo, BranchKind::Local).await.ok();
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
                let paused = (!matches!(step, OperationStep::Abort))
                    .then(|| this.status.as_ref().and_then(|s| s.operation))
                    .flatten();
                if let Some(next_operation) = paused {
                    info!(?operation, ?step, ?next_operation, "vcs: op step paused");
                    this.handle_operation_paused(next_operation, cx);
                    this.refresh_after_head_change(cx);
                } else if let Err(e) = result {
                    error!(error = %e, ?operation, ?step, "vcs: op step failed");
                    this.error = Some(format!(
                        "{}·{}失败：{e}",
                        operation_label(operation),
                        step_label(step)
                    ));
                } else {
                    info!(?operation, ?step, "vcs: op step done");
                    // 继续 = 产生新 commit / 推进 rebase；中止 = 回滚工作区。HEAD 内容都变了
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    let done = match step {
                        OperationStep::Continue => "已继续",
                        OperationStep::Skip => "已跳过当前 commit",
                        OperationStep::Abort => "已中止",
                    };
                    this.notify_success(format!("{}{done}", operation_label(operation)), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 加载交互式 rebase 计划并显示编辑器
    pub(super) fn start_interactive_rebase(&mut self, onto: String, cx: &mut Context<Self>) {
        if !self.ensure_no_operation("开始新的 Rebase", cx) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        let onto_clone = onto.clone();
        self.loading_rebase_plan = true;
        self.rebase_plan_onto = onto;
        self.show_rebase_plan = true;
        self.error = None;
        self.rebase_request_seq = self.rebase_request_seq.wrapping_add(1);
        let request_seq = self.rebase_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.interactive_rebase_plan(&repo, &onto_clone).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.rebase_request_seq != request_seq {
                    return;
                }
                this.loading_rebase_plan = false;
                match result {
                    Ok(todos) => this.rebase_todos = todos,
                    Err(e) => {
                        error!(error = %e, onto = %onto_clone, "vcs: load rebase plan failed");
                        this.error = Some(format!("加载 rebase 计划失败：{e}"));
                        this.show_rebase_plan = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 执行当前编辑好的 rebase 计划
    pub(super) fn execute_interactive_rebase(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        let onto = self.rebase_plan_onto.clone();
        let todos: Vec<ramag_domain::entities::RebaseTodo> = self.rebase_todos.clone();
        if !self.begin_op("Rebase 中…", cx) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = driver
                .interactive_rebase_execute(&repo, &onto, &todos)
                .await;
            let new_status = driver.status(&repo).await.ok();
            let new_local = driver.list_branches(&repo, BranchKind::Local).await.ok();
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.busy_label = None;
                if !this.is_current_repo(&repo) {
                    cx.notify();
                    return;
                }
                this.show_rebase_plan = false;
                this.rebase_todos.clear();
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                if let Some(branches) = new_local {
                    this.local_branches = branches;
                }
                if matches!(
                    this.status.as_ref().and_then(|s| s.operation),
                    Some(ramag_domain::entities::RepoOperation::Rebase)
                ) {
                    // 冲突或 edit 暂停都保持 operation=Rebase；无论 driver 返回 Ok/Err 都按暂停处理。
                    info!(%onto, "vcs: interactive rebase paused");
                    this.handle_operation_paused(RepoOperation::Rebase, cx);
                    this.refresh_after_head_change(cx);
                } else if let Err(e) = result {
                    error!(error = %e, %onto, "vcs: interactive rebase failed");
                    this.error = Some(format!("交互式 Rebase 失败：{e}"));
                } else {
                    info!(%onto, "vcs: interactive rebase done");
                    // 历史被改写：history 与所有 diff 缓存都要重建
                    this.load_history_page(0, cx);
                    this.refresh_after_head_change(cx);
                    this.notify_success("交互式 Rebase 完成", cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
