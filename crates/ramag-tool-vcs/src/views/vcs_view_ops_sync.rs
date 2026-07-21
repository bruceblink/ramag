//! 工作区状态同步：静默刷新（窗口激活 / 手动刷新）+ Changes 文件 tabs 与最新 status 对齐

use std::collections::{HashMap, HashSet};

use gpui::Context;
use ramag_domain::entities::{FileChangeKind, FileStatus};

use super::helpers::{FileTabSource, GroupKind};
use super::vcs_view::VcsView;
use crate::watcher::RepoRefresh;

mod merge;

use self::merge::{
    merge_partial_project_files, merge_partial_status, path_matches_prefixes, status_changes,
};

enum WorkspaceStatusResult {
    Full(ramag_domain::error::Result<ramag_domain::entities::WorkingTreeStatus>),
    Partial {
        paths: Vec<String>,
        result: ramag_domain::error::Result<Vec<FileStatus>>,
    },
}

/// 写操作后的辅助刷新允许保留旧 UI 数据，但失败必须留有可定位日志。
pub(super) fn best_effort_refresh<T>(
    result: ramag_domain::error::Result<T>,
    resource: &'static str,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(error = %error, resource, "vcs post-operation refresh failed");
            None
        }
    }
}

impl VcsView {
    /// 静默刷新工作区：status + 本地/远程分支 + 当前 Files 视图数据。
    /// 不显示整屏 loading；完成后对齐 Changes tabs（外部改动 / 终端 git 操作后界面自动跟上）
    pub(super) fn refresh_workspace_silent(&mut self, cx: &mut Context<Self>) {
        self.refresh_workspace(RepoRefresh::full(), cx);
    }

    fn refresh_workspace_change(&mut self, refresh: RepoRefresh, cx: &mut Context<Self>) {
        if !refresh.is_empty() {
            self.refresh_workspace(refresh, cx);
        }
    }

    fn refresh_workspace(&mut self, refresh: RepoRefresh, cx: &mut Context<Self>) {
        if !begin_workspace_refresh(
            &mut self.workspace_refresh_in_flight,
            &mut self.workspace_refresh_pending,
            refresh.clone(),
        ) {
            return;
        }
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            self.workspace_refresh_in_flight = false;
            return;
        };
        self.status_request_seq = self.status_request_seq.wrapping_add(1);
        let request_seq = self.status_request_seq;
        let driver = self.driver.clone();
        let refresh_project_files =
            matches!(self.files_view_mode, super::helpers::FilesViewMode::Project)
                && self.project_files_version > 0
                && !self.loading_project_files;
        cx.spawn(async move |this, cx| {
            let RepoRefresh {
                full_status,
                refresh_refs,
                paths,
            } = refresh;
            let status_driver = driver.clone();
            let status_repo = repo.clone();
            let project_paths = (!full_status && refresh_project_files).then(|| paths.clone());
            let status_fut = async move {
                if full_status {
                    WorkspaceStatusResult::Full(status_driver.status(&status_repo).await)
                } else {
                    let result = status_driver.status_paths(&status_repo, &paths).await;
                    WorkspaceStatusResult::Partial { paths, result }
                }
            };
            let branch_repo = repo.clone();
            let branch_driver = driver.clone();
            let branches_fut = async move {
                if refresh_refs {
                    Some(branch_driver.list_all_branches(&branch_repo).await)
                } else {
                    None
                }
            };
            let project_repo = repo.clone();
            let project_files_fut = async move {
                match project_paths {
                    Some(paths) => {
                        let result = driver.list_files_paths(&project_repo, &paths).await;
                        Some((paths, result))
                    }
                    None => None,
                }
            };
            let (status_result, branches, project_files) =
                futures::join!(status_fut, branches_fut, project_files_fut);
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) {
                    return;
                }
                this.workspace_refresh_in_flight = false;
                let mut next_refresh = std::mem::take(&mut this.workspace_refresh_pending);
                if this.status_request_seq != request_seq {
                    if should_rerun(this, &next_refresh) {
                        this.refresh_workspace(next_refresh, cx);
                    }
                    return;
                }
                let event_paths = match &status_result {
                    WorkspaceStatusResult::Full(_) => None,
                    WorkspaceStatusResult::Partial { paths, .. } => Some(paths.clone()),
                };
                let (files_changed, head_changed) = match status_result {
                    WorkspaceStatusResult::Full(status) => match status {
                        Ok(status) => {
                            let changes = this
                                .status
                                .as_ref()
                                .map_or((true, false), |old| status_changes(old, &status));
                            this.status = Some(status);
                            changes
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "vcs: workspace status refresh failed");
                            this.error = Some(format!("刷新工作区状态失败：{error}"));
                            (false, false)
                        }
                    },
                    WorkspaceStatusResult::Partial { paths, result } => match result {
                        Ok(files) => match this.status.as_mut() {
                            Some(status) => (merge_partial_status(status, &paths, files), false),
                            None => {
                                // 缺少完整基线时不能安全合并，补一次完整刷新。
                                next_refresh.merge(RepoRefresh::full());
                                (false, false)
                            }
                        },
                        Err(error) => {
                            tracing::warn!(error = %error, "vcs: incremental workspace status refresh failed");
                            this.error = Some(format!("增量刷新工作区状态失败：{error}"));
                            (false, false)
                        }
                    },
                };
                let mut project_files_incremental = false;
                if let Some((paths, result)) = project_files {
                    project_files_incremental = true;
                    match result {
                        Ok(files) => {
                            if merge_partial_project_files(&mut this.project_files, &paths, files) {
                                this.prune_project_expanded_dirs();
                                this.project_files_version =
                                    this.project_files_version.wrapping_add(1);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "vcs: incremental project files refresh failed");
                            this.error = Some(format!("增量刷新 Project Files 失败：{error}"));
                            this.reload_project_files(cx);
                        }
                    }
                }
                if let Some(branches) = branches {
                    match branches {
                        Ok((local, remote)) => {
                            this.local_branches = local;
                            this.remote_branches = remote;
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "vcs: branch refresh failed");
                            this.error = Some(format!("刷新分支失败：{error}"));
                        }
                    }
                }
                if head_changed {
                    // 两个分支都干净时 files 指纹同为空，但文件内容仍可能完全不同。
                    this.refresh_after_head_change(cx);
                    if this.history_pane_visible || !this.history_commits.is_empty() {
                        this.load_history_page(0, cx);
                    }
                } else {
                    let full_status_refresh = event_paths.is_none();
                    let event_prefixes = event_paths
                        .as_ref()
                        .map(|paths| paths.iter().map(String::as_str).collect::<HashSet<_>>());
                    // watcher 明确给出路径时只失效命中标签；全量 status 变化才失效全部。
                    for tab in &mut this.file_tabs {
                        if matches!(tab.source, FileTabSource::ProjectFiles)
                            && ((full_status_refresh && files_changed)
                                || event_prefixes
                                    .as_ref()
                                    .is_some_and(|prefixes| path_matches_prefixes(&tab.path, prefixes)))
                        {
                            tab.cached_content = None;
                        }
                    }
                    if files_changed {
                        this.sync_changes_tabs_with_status_paths(event_paths.as_deref(), cx);
                    } else if let Some(prefixes) = event_prefixes.as_ref()
                        && let Some(idx) = this.active_file_tab_idx
                        && let Some(tab) = this.file_tabs.get(idx).cloned()
                        && path_matches_prefixes(&tab.path, prefixes)
                        && let FileTabSource::Changes(kind) = tab.source
                    {
                        // 状态种类不变不代表内容没变；仅重拉命中的活动 diff。
                        if let Some(active_tab) = this.file_tabs.get_mut(idx) {
                            active_tab.cached_diff = None;
                            active_tab.cached_diff_syntax = None;
                        }
                        this.select_file(tab.path, kind, cx);
                    }
                    // active 是 PF tab 时立即重读。
                    if let Some(idx) = this.active_file_tab_idx
                        && let Some(tab) = this.file_tabs.get(idx).cloned()
                        && matches!(tab.source, FileTabSource::ProjectFiles)
                        && ((full_status_refresh && files_changed)
                            || event_prefixes.as_ref().is_some_and(|prefixes| {
                                path_matches_prefixes(&tab.path, prefixes)
                            }))
                    {
                        this.select_pf_file(tab.path, cx);
                    }
                    if files_changed {
                        // 全量刷新没有路径补丁时，Project Files 仍需重拉。
                        match this.files_view_mode {
                            super::helpers::FilesViewMode::Project
                                if !project_files_incremental =>
                            {
                                this.reload_project_files(cx);
                            }
                            super::helpers::FilesViewMode::Stash => this.reload_stashes(cx),
                            _ => {}
                        }
                    }
                }
                cx.notify();
                if should_rerun(this, &next_refresh) {
                    this.refresh_workspace(next_refresh, cx);
                }
            });
        })
        .detach();
    }

    /// 启动当前仓库的文件系统监听：外部改动防抖后静默刷新。
    /// 旧 watcher 先 drop（防抖线程随通道关闭退出，旧转发任务随 sender 关闭结束）
    pub(in crate::views) fn start_fs_watcher(&mut self, cx: &mut Context<Self>) {
        self.fs_watcher = None;
        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let root = std::path::PathBuf::from(&repo.path);
        // futures mpsc 每个 sender 自带一个保留槽；容量 0 + 单 sender 即最多积压一个刷新信号。
        let (tx, mut rx) = futures::channel::mpsc::channel::<()>(0);
        let tx = std::sync::Arc::new(std::sync::Mutex::new(tx));
        let pending = std::sync::Arc::new(std::sync::Mutex::new(RepoRefresh::default()));
        let tx_for_watcher = tx.clone();
        let pending_for_watcher = pending.clone();
        match crate::watcher::RepoWatcher::start(root, move |refresh| {
            merge_pending_refresh(&pending_for_watcher, refresh);
            enqueue_workspace_refresh(&tx_for_watcher);
        }) {
            Ok(w) => {
                self.fs_watcher = Some(w);
                cx.spawn(async move |this, cx| {
                    use futures::StreamExt as _;
                    while rx.next().await.is_some() {
                        let refresh = take_pending_refresh(&pending);
                        let alive = this.update(cx, |this, cx| {
                            // busy 中跳过：写操作完成路径自己会刷新，避免叠加
                            if this.repo.is_some() && !this.loading && !this.busy {
                                this.refresh_workspace_change(refresh, cx);
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => {
                // 监听失败不阻断使用：窗口激活刷新 + 手动刷新仍可用
                tracing::warn!(error = %e, "vcs: fs watcher start failed");
            }
        }
    }

    /// 把 Changes 来源的文件 tabs 与最新 `self.status` 对齐：
    /// - 文件已无任何变更 → 关闭 tab（diff 必为空，保留无意义）
    /// - 文件变更组别迁移（如 stage 后 Unstaged → Staged）→ 重定向 tab 的 GroupKind
    /// - 全量刷新清全部 Changes 缓存；watcher 增量刷新只清命中路径并重拉命中的 active tab
    ///
    /// ProjectFiles / Commit 来源的 tabs 不受影响（仅索引可能因关闭前移）
    pub(super) fn sync_changes_tabs_with_status(&mut self, cx: &mut Context<Self>) {
        self.sync_changes_tabs_with_status_paths(None, cx);
    }

    /// watcher 增量刷新只失效命中路径的 Changes 缓存；`paths=None` 表示全量失效。
    fn sync_changes_tabs_with_status_paths(
        &mut self,
        paths: Option<&[String]>,
        cx: &mut Context<Self>,
    ) {
        self.prune_changes_collapsed_dirs();
        let Some(status) = self.status.as_ref() else {
            return;
        };
        let affected_prefixes =
            paths.map(|paths| paths.iter().map(String::as_str).collect::<HashSet<_>>());
        let is_affected = |path: &str| {
            affected_prefixes
                .as_ref()
                .is_none_or(|prefixes| path_matches_prefixes(path, prefixes))
        };
        // 没有 Changes 来源标签时无需复制或索引可能很大的工作区文件列表。
        if !self
            .file_tabs
            .iter()
            .any(|tab| matches!(tab.source, FileTabSource::Changes(_)))
        {
            return;
        }
        let active_identity = self
            .active_file_tab_idx
            .and_then(|i| self.file_tabs.get(i))
            .map(|t| (t.path.clone(), t.source.clone()));

        // 预建借用索引，把 O(tabs × files) 的重复线性搜索降为 O(files + tabs)，
        // 同时不克隆路径与整份 FileStatus。
        let files_by_path: HashMap<&str, &FileStatus> = status
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let mut new_tabs = Vec::with_capacity(self.file_tabs.len());
        for mut tab in std::mem::take(&mut self.file_tabs) {
            let FileTabSource::Changes(kind) = tab.source else {
                new_tabs.push(tab);
                continue;
            };
            let Some(f) = files_by_path.get(tab.path.as_str()).copied() else {
                continue;
            };
            let new_kind = redirect_group_kind(f, kind);
            // 重定向后可能与既有 tab 重合（如 Staged + Unstaged 两个 tab 合流）→ 去重
            if new_tabs.iter().any(|t: &super::helpers::FileTab| {
                t.path == tab.path && t.source == FileTabSource::Changes(new_kind)
            }) {
                continue;
            }
            tab.source = FileTabSource::Changes(new_kind);
            if is_affected(&tab.path) {
                tab.cached_diff = None;
                tab.cached_diff_syntax = None;
            }
            new_tabs.push(tab);
        }
        self.file_tabs = new_tabs;

        // 恢复 active tab：优先同 (path, source)，其次同 path 的 Changes tab，再次序号回退
        let restored = active_identity.and_then(|(path, source)| {
            self.file_tabs
                .iter()
                .position(|t| t.path == path && t.source == source)
                .or_else(|| {
                    matches!(source, FileTabSource::Changes(_))
                        .then(|| {
                            self.file_tabs.iter().position(|t| {
                                t.path == path && matches!(t.source, FileTabSource::Changes(_))
                            })
                        })
                        .flatten()
                })
        });
        match restored {
            Some(idx) => {
                self.active_file_tab_idx = Some(idx);
                let tab = self.file_tabs[idx].clone();
                match tab.source {
                    FileTabSource::Changes(kind)
                        if is_affected(&tab.path) || tab.cached_diff.is_none() =>
                    {
                        self.select_file(tab.path, kind, cx);
                    }
                    // 其余来源缓存未动，仅同步派生字段
                    _ => self.activate_file_tab_state(tab),
                }
            }
            None => {
                // active tab 被关：顺延到最后一个 tab；没有 tab 则清空主区
                self.active_file_tab_idx = self.file_tabs.len().checked_sub(1);
                if let Some(idx) = self.active_file_tab_idx {
                    let tab = self.file_tabs[idx].clone();
                    match tab.source {
                        FileTabSource::Changes(kind)
                            if is_affected(&tab.path) || tab.cached_diff.is_none() =>
                        {
                            self.select_file(tab.path, kind, cx);
                        }
                        _ => self.activate_file_tab_state(tab),
                    }
                } else {
                    self.selected_file = None;
                    self.current_diff = None;
                    self.current_diff_syntax = None;
                    self.loading_diff = false;
                    self.selected_pf_path = None;
                    self.current_file_content = None;
                    self.loading_file_content = false;
                    self.selected_commit_file = None;
                }
            }
        }
        cx.notify();
    }
}

fn begin_workspace_refresh(
    in_flight: &mut bool,
    pending: &mut RepoRefresh,
    refresh: RepoRefresh,
) -> bool {
    if *in_flight {
        pending.merge(refresh);
        false
    } else {
        *in_flight = true;
        true
    }
}

fn should_rerun(this: &VcsView, pending: &RepoRefresh) -> bool {
    this.repo.is_some() && !this.loading && !this.busy && !pending.is_empty()
}

fn merge_pending_refresh(pending: &std::sync::Mutex<RepoRefresh>, refresh: RepoRefresh) {
    match pending.lock() {
        Ok(mut pending) => pending.merge(refresh),
        Err(error) => {
            tracing::warn!("vcs workspace refresh state lock poisoned");
            error.into_inner().merge(refresh);
        }
    }
}

fn take_pending_refresh(pending: &std::sync::Mutex<RepoRefresh>) -> RepoRefresh {
    match pending.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(error) => {
            tracing::warn!("vcs workspace refresh state lock poisoned");
            let mut pending = error.into_inner();
            std::mem::take(&mut *pending)
        }
    }
}

fn enqueue_workspace_refresh(sender: &std::sync::Mutex<futures::channel::mpsc::Sender<()>>) {
    let mut sender = match sender.lock() {
        Ok(sender) => sender,
        Err(_) => {
            tracing::warn!("vcs workspace refresh channel lock poisoned");
            return;
        }
    };
    match sender.try_send(()) {
        Ok(()) => {}
        Err(error) if error.is_full() || error.is_disconnected() => {}
        Err(error) => {
            tracing::warn!(error = %error, "vcs workspace refresh enqueue failed");
        }
    }
}

/// 按最新文件状态推导 tab 应归属的组：原组仍有效则保持，否则按 冲突 > 已暂存 > 未暂存 > 未跟踪 迁移
fn redirect_group_kind(f: &FileStatus, prefer: GroupKind) -> GroupKind {
    if f.is_conflicted() {
        return GroupKind::Conflict;
    }
    let staged_ok = f.staged.is_some();
    let untracked = matches!(f.unstaged, Some(FileChangeKind::Untracked));
    let unstaged_ok = f.unstaged.is_some() && !untracked;
    let valid = |k: GroupKind| match k {
        GroupKind::Staged => staged_ok,
        GroupKind::Unstaged => unstaged_ok,
        GroupKind::Untracked => untracked,
        GroupKind::Conflict => false,
    };
    if valid(prefer) {
        prefer
    } else if staged_ok {
        GroupKind::Staged
    } else if unstaged_ok {
        GroupKind::Unstaged
    } else {
        GroupKind::Untracked
    }
}

#[cfg(test)]
#[path = "vcs_view_ops_sync/tests.rs"]
mod tests;
