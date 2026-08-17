mod new;
mod render;
#[cfg(test)]
mod render_test;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, SharedString,
    UniformListScrollHandle,
};
use gpui_component::{input::InputState, resizable::ResizableState};
use ramag_domain::entities::{
    Branch, Commit, ConflictContent, FileDiff, FileStatus, RebaseTodo, Remote, RepoConfig, RepoId,
    Stash, Tag, WorkingTreeStatus,
};
use ramag_domain::traits::{GitDriver, Storage};
use ramag_ui::AxisScrollGesture;

use super::commit_detail::CommitFilesRowsCacheEntry;
use super::helpers::{
    ActiveView, DiffViewMode, FileContentSnapshot, FileTab, FileTabSource, FilesViewMode,
    GroupKind, PendingFileEditorLoad, ViewMode,
};
use super::history_panel::HistoryLeftRowsCacheEntry;
use super::project_files::{ProjectRowsCacheEntry, ProjectStatusCacheEntry};
use super::reflog_view::ReflogRowsCacheEntry;
use super::repo_list::RepoListRowsCacheEntry;
use super::workspace_panel::WorkspaceRowsCacheEntry;

#[derive(Clone, Default)]
pub(super) struct RepoSessionState {
    pub file_tabs: Vec<FileTab>,
    pub active_file_tab_idx: Option<usize>,
    pub commit_text: SharedString,
    pub commit_amend: bool,
    pub commit_sign: bool,
    pub ide_left_resize: Option<Entity<ResizableState>>,
    pub ide_files_resize: Option<Entity<ResizableState>>,
    pub detail_resize: Option<Entity<ResizableState>>,
}

#[derive(Debug, Clone)]
pub enum VcsEvent {
    OpenRepo(PathBuf),
}

pub struct VcsView {
    pub(super) driver: Arc<dyn GitDriver>,
    pub(super) storage: Arc<dyn Storage>,
    /// 按路径保留最新仓库写入并串行落盘，防止旧状态覆盖。
    pub(super) repo_write_coordinator: super::latest_write::LatestWriteCoordinator,
    pub(super) repo: Option<RepoConfig>,
    pub(super) status: Option<WorkingTreeStatus>,
    pub(super) status_request_seq: u64,
    /// 状态与分支刷新仅允许一组在途；新请求合并后补刷。
    pub(super) workspace_refresh_in_flight: bool,
    pub(super) workspace_refresh_pending: crate::watcher::RepoRefresh,
    pub(super) local_branches: Vec<Branch>,
    pub(super) remote_branches: Vec<Branch>,
    pub(super) error: Option<String>,
    pub(super) loading: bool,
    pub(super) loading_label: Option<String>,
    pub(super) clone_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(super) clone_progress: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    pub(super) pending_clone_cleanup: Option<std::path::PathBuf>,
    pub(super) busy: bool,
    pub(super) busy_label: Option<&'static str>,
    pub(super) remote_op_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(super) remote_op_progress: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// 异步回调无 Window，由渲染层延后推送通知。
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    pub(super) was_window_active: bool,
    pub(super) commit_input: Entity<InputState>,
    pub(super) commit_amend: bool,
    pub(super) commit_sign: bool,
    /// 切仓后待恢复的提交草稿，由渲染层延后写入输入框。
    pub(super) pending_commit_text: Option<SharedString>,
    pub(super) commit_draft_gen: Arc<std::sync::atomic::AtomicU64>,
    /// 提交草稿串行写入，防止旧写覆盖新内容。
    pub(super) commit_draft_write_lock: Arc<futures::lock::Mutex<()>>,
    pub(super) commit_draft_error: Option<String>,
    pub(super) pending_clear_search_inputs: bool,
    pub(super) pending_clear_creation_inputs: bool,
    pub(super) selected_file: Option<(String, GroupKind)>,
    pub(super) current_diff: Option<std::rc::Rc<FileDiff>>,
    /// 当前 diff 的语法树，滚动时仅查询可见范围。
    pub(super) current_diff_syntax: Option<std::rc::Rc<super::syntax::DiffSyntaxSnapshot>>,
    pub(super) diff_layout_cache: RefCell<Option<super::diff_panel_split::DiffLayoutCacheEntry>>,
    pub(super) loading_diff: bool,
    /// diff 请求代际，防止旧回包覆盖。
    pub(super) diff_request_seq: u64,
    pub(super) view_mode: ViewMode,
    pub(super) history_commits: std::rc::Rc<Vec<std::rc::Rc<Commit>>>,
    pub(super) history_retained_bytes: usize,
    /// 达到条数或内存上限后停止自动翻页。
    pub(super) history_limit_reached: bool,
    pub(super) history_graph_rows: std::rc::Rc<Vec<super::commit_graph::CommitGraphRow>>,
    pub(super) history_graph_state: super::commit_graph::CommitLaneState,
    pub(super) history_has_more: bool,
    pub(super) history_request_seq: u64,
    pub(super) loading_history: bool,
    pub(super) stashes: Vec<Stash>,
    pub(super) loading_stashes: bool,
    pub(super) stash_request_seq: u64,
    pub(super) create_branch_input: Entity<InputState>,
    pub(super) create_branch_base: Option<String>,
    pub(super) tags: Vec<Tag>,
    pub(super) loading_tags: bool,
    pub(super) tag_request_seq: u64,
    pub(super) create_tag_input: Entity<InputState>,
    pub(super) create_tag_message_input: Entity<InputState>,
    pub(super) collapsed_local: bool,
    pub(super) collapsed_remote: bool,
    pub(super) collapsed_tag: bool,
    pub(super) collapsed_remote_repos: bool,
    pub(super) history_left_rows_cache: RefCell<Option<HistoryLeftRowsCacheEntry>>,
    pub(super) expanded_diff_spacers: std::collections::HashSet<(usize, usize)>,
    pub(super) remotes: Vec<Remote>,
    pub(super) loading_remotes: bool,
    pub(super) remotes_request_seq: u64,
    pub(super) create_remote_name_input: Entity<InputState>,
    pub(super) create_remote_url_input: Entity<InputState>,
    pub(super) viewing_commit: Option<std::rc::Rc<Commit>>,
    pub(super) commit_files: std::rc::Rc<Vec<FileStatus>>,
    pub(super) selected_commit_file: Option<String>,
    pub(super) commit_file_diff: Option<std::rc::Rc<FileDiff>>,
    pub(super) loading_commit_files: bool,
    /// 提交详情请求代际，防止旧回包覆盖。
    pub(super) commit_detail_request_seq: u64,
    pub(super) commit_files_collapsed: std::collections::HashSet<String>,
    pub(super) commit_files_collapsed_version: u64,
    pub(super) commit_files_rows_cache: RefCell<Option<CommitFilesRowsCacheEntry>>,
    pub(super) changes_collapsed_dirs: std::collections::HashSet<String>,
    pub(super) changes_collapsed_dirs_version: u64,
    pub(super) changes_rows_cache: RefCell<Option<WorkspaceRowsCacheEntry>>,
    pub(super) history_path_filter: Option<String>,
    pub(super) history_search_input: Entity<InputState>,
    /// blame 行数据，以 Rc 供 diff 共享。
    pub(super) blame_lines: std::rc::Rc<Vec<ramag_domain::entities::BlameLine>>,
    pub(super) loading_blame: bool,
    /// 完整和行级 blame 分别计代，互不干扰。
    pub(super) blame_request_seq: u64,
    pub(super) inline_blame_request_seq: u64,
    pub(super) showing_blame: bool,
    /// 行级 blame 文本；点击行号显示，关闭则清空。
    pub(super) inline_blame_text: Option<SharedString>,
    pub(super) diff_view_mode: DiffViewMode,
    pub(super) reflog_entries: std::rc::Rc<Vec<ramag_domain::entities::ReflogEntry>>,
    pub(super) reflog_rows_cache: RefCell<Option<ReflogRowsCacheEntry>>,
    pub(super) loading_reflog: bool,
    pub(super) reflog_request_seq: u64,
    pub(super) showing_reflog: bool,
    pub(super) ide_left_resize: Entity<ResizableState>,
    pub(super) ide_left_width: f32,
    pub(super) ide_files_resize: Entity<ResizableState>,
    pub(super) detail_resize: Entity<ResizableState>,
    pub(super) active_view: ActiveView,
    pub(super) recent_repos: std::rc::Rc<Vec<RepoConfig>>,
    pub(super) repo_list_rows_cache: RefCell<Option<RepoListRowsCacheEntry>>,
    pub(super) repo_search_input: Entity<InputState>,
    pub(super) focused_repo_search_once: bool,
    pub(super) files_view_mode: FilesViewMode,
    pub(super) files_search_input: Entity<InputState>,
    pub(super) project_files: Vec<String>,
    pub(super) loading_project_files: bool,
    pub(super) project_files_request_seq: u64,
    pub(super) project_expanded_dirs: std::collections::HashSet<String>,
    pub(super) project_files_version: u64,
    pub(super) project_expanded_dirs_version: u64,
    pub(super) project_rows_cache: RefCell<Option<ProjectRowsCacheEntry>>,
    pub(super) project_status_cache: RefCell<Option<ProjectStatusCacheEntry>>,
    pub(super) project_scroll: UniformListScrollHandle,
    pub(super) selected_pf_path: Option<String>,
    pub(super) current_file_content: Option<FileContentSnapshot>,
    pub(super) pf_editor: Entity<InputState>,
    /// 异步读完后由渲染层写入编辑器。
    pub(super) pending_pf_editor_load: Option<PendingFileEditorLoad>,
    pub(super) pf_editor_loaded_path: Option<String>,
    pub(super) pf_editor_dirty: bool,
    pub(super) pf_show_source: bool,
    /// 编辑代际，防止自动保存误标后续修改。
    pub(super) pf_editor_revision: u64,
    pub(super) pf_editor_line_count: usize,
    pub(super) loading_file_content: bool,
    /// 文件内容请求代际，防止旧读盘结果覆盖。
    pub(super) file_content_request_seq: u64,
    /// 自动保存按仓库和路径保留最新写入，并串行执行。
    pub(super) project_file_write_coordinator: super::latest_write::LatestWriteCoordinator,
    /// 最近自身写入，用于忽略相应 watcher 事件。
    pub(super) project_file_self_writes:
        std::collections::HashMap<String, (u64, std::time::Instant)>,
    pub(super) diff_scroll: UniformListScrollHandle,
    pub(super) commit_files_scroll: UniformListScrollHandle,
    pub(super) changes_scroll: UniformListScrollHandle,
    pub(super) history_left_scroll: UniformListScrollHandle,
    pub(super) conflict_ours_scroll: UniformListScrollHandle,
    pub(super) conflict_theirs_scroll: UniformListScrollHandle,
    pub(super) history_scroll: UniformListScrollHandle,
    pub(super) reflog_scroll: UniformListScrollHandle,
    pub(super) stash_scroll: UniformListScrollHandle,
    pub(super) rebase_scroll: UniformListScrollHandle,
    pub(super) file_tabs_h_scroll: ScrollHandle,
    pub(super) diff_h_scroll: ScrollHandle,
    pub(super) diff_scroll_gesture: AxisScrollGesture,
    pub(super) history_pane_visible: bool,
    pub(super) diff_fullscreen: bool,

    pub(super) open_repos: Vec<RepoConfig>,
    /// 仅在用户未操作仓库时恢复启动标签。
    pub(super) startup_repo_restore_allowed: bool,
    pub(super) repos_scroll: ScrollHandle,
    pub(super) file_tabs: Vec<FileTab>,
    pub(super) active_file_tab_idx: Option<usize>,
    pub(super) repo_session_cache: std::collections::HashMap<String, RepoSessionState>,
    pub(super) repo_session_order: std::collections::VecDeque<String>,

    pub(super) clone_url_input: Entity<InputState>,
    pub(super) clone_dest_path: Option<PathBuf>,
    /// 系统目录选择器闸门，期间禁用目录入口。
    pub(super) directory_picker_busy: bool,

    pub(super) show_rebase_plan: bool,
    pub(super) rebase_plan_onto: String,
    pub(super) rebase_todos: Vec<RebaseTodo>,
    pub(super) loading_rebase_plan: bool,
    pub(super) rebase_request_seq: u64,

    pub(super) conflict_editor_path: Option<String>,
    pub(super) conflict_content: Option<std::rc::Rc<ConflictContent>>,
    pub(super) loading_conflict: bool,
    /// 冲突内容请求代际，只接收最后一次回包。
    pub(super) conflict_request_seq: u64,

    pub(super) fs_watcher: Option<crate::watcher::RepoWatcher>,

    pub(super) focus_handle: FocusHandle,
}

impl EventEmitter<VcsEvent> for VcsView {}

impl Focusable for VcsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl VcsView {
    pub(super) fn is_current_repo(&self, repo_id: &RepoId) -> bool {
        self.repo.as_ref().map(|r| &r.id) == Some(repo_id)
    }

    /// 检查 Git 操作状态；冲突处理入口不走此闸门。
    pub(super) fn ensure_no_operation(&mut self, action: &str, cx: &mut Context<Self>) -> bool {
        let Some(operation) = self.status.as_ref().and_then(|status| status.operation) else {
            return true;
        };
        self.error = Some(format!(
            "{}仍在进行中：请先点顶部「继续」或「中止」，再{action}",
            super::helpers::operation_label(operation)
        ));
        cx.notify();
        false
    }

    pub(super) fn is_working_tree_dirty(&self) -> bool {
        self.status
            .as_ref()
            .map(|s| {
                s.files
                    .iter()
                    .any(|f| f.staged.is_some() || f.unstaged.is_some())
            })
            .unwrap_or(false)
    }

    pub(super) fn set_files_view_mode(&mut self, mode: FilesViewMode, cx: &mut Context<Self>) {
        if self.files_view_mode != mode {
            self.capture_active_project_draft(cx);
            self.files_view_mode = mode;
            if !matches!(mode, FilesViewMode::Project) {
                self.selected_pf_path = None;
                self.current_file_content = None;
                self.loading_file_content = false;
                self.project_rows_cache.get_mut().take();
                self.project_status_cache.get_mut().take();
            } else {
                self.selected_file = None;
                self.current_diff = None;
                self.current_diff_syntax = None;
                self.loading_diff = false;
            }
            if !matches!(mode, FilesViewMode::Changes) {
                self.changes_rows_cache.get_mut().take();
            }
            cx.notify();
            self.refresh_current_files_view(cx);
        }
    }

    pub(super) fn show_repo_list(&mut self, cx: &mut Context<Self>) {
        self.active_view = ActiveView::RepoList;
        self.diff_layout_cache.get_mut().take();
        cx.notify();
    }

    pub(super) fn clear_session_data(&mut self) {
        self.selected_file = None;
        self.current_diff = None;
        self.current_diff_syntax = None;
        self.diff_layout_cache.get_mut().take();
        self.loading_diff = false;
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        self.selected_pf_path = None;
        self.current_file_content = None;
        self.pending_pf_editor_load = None;
        self.pf_editor_loaded_path = None;
        self.pf_editor_dirty = false;
        self.pf_editor_revision = 0;
        self.pf_editor_line_count = 0;
        self.loading_file_content = false;
        self.file_content_request_seq = self.file_content_request_seq.wrapping_add(1);
        self.project_file_self_writes.clear();
        self.file_tabs_h_scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        self.diff_fullscreen = false;
        self.viewing_commit = None;
        self.reset_commit_files_tree();
        self.changes_collapsed_dirs.clear();
        self.changes_collapsed_dirs_version = self.changes_collapsed_dirs_version.wrapping_add(1);
        self.changes_rows_cache.get_mut().take();
        self.selected_commit_file = None;
        self.commit_file_diff = None;
        self.loading_commit_files = false;
        self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
        self.show_rebase_plan = false;
        self.rebase_todos.clear();
        self.rebase_scroll = UniformListScrollHandle::new();
        self.loading_rebase_plan = false;
        self.rebase_request_seq = self.rebase_request_seq.wrapping_add(1);
        self.conflict_editor_path = None;
        self.conflict_content = None;
        self.loading_conflict = false;
        self.set_history_commits(Vec::new());
        self.history_has_more = false;
        self.loading_history = false;
        self.history_request_seq = self.history_request_seq.wrapping_add(1);
        self.project_files.clear();
        self.project_status_cache.get_mut().take();
        self.loading_project_files = false;
        self.project_files_request_seq = self.project_files_request_seq.wrapping_add(1);
        self.project_expanded_dirs.clear();
        self.file_tabs.clear();
        self.active_file_tab_idx = None;
        self.history_path_filter = None;
        self.reflog_entries = std::rc::Rc::new(Vec::new());
        self.reflog_rows_cache.get_mut().take();
        self.showing_reflog = false;
        self.loading_reflog = false;
        self.reflog_request_seq = self.reflog_request_seq.wrapping_add(1);
        self.blame_lines = std::rc::Rc::new(Vec::new());
        self.showing_blame = false;
        self.loading_blame = false;
        self.blame_request_seq = self.blame_request_seq.wrapping_add(1);
        self.inline_blame_request_seq = self.inline_blame_request_seq.wrapping_add(1);
        self.inline_blame_text = None;
        self.expanded_diff_spacers.clear();
        self.error = None;
        self.conflict_request_seq = self.conflict_request_seq.wrapping_add(1);
        self.stashes.clear();
        self.stash_scroll = UniformListScrollHandle::new();
        self.loading_stashes = false;
        self.stash_request_seq = self.stash_request_seq.wrapping_add(1);
        self.tags.clear();
        self.loading_tags = false;
        self.tag_request_seq = self.tag_request_seq.wrapping_add(1);
        self.remotes.clear();
        self.loading_remotes = false;
        self.remotes_request_seq = self.remotes_request_seq.wrapping_add(1);
        self.history_left_rows_cache.get_mut().take();
        self.status_request_seq = self.status_request_seq.wrapping_add(1);
        self.workspace_refresh_in_flight = false;
        self.workspace_refresh_pending = Default::default();
        // InputState 需要 Window，由 Render 延后清空。
        self.pending_clear_search_inputs = true;
        self.pending_clear_creation_inputs = true;
        // 全局操作状态和 diff 选项跨仓保留。
    }

    pub(super) fn toggle_history_pane(&mut self, cx: &mut Context<Self>) {
        self.history_pane_visible = !self.history_pane_visible;
        if self.history_pane_visible
            && self.history_commits.is_empty()
            && !self.loading_history
            && self.repo.is_some()
        {
            self.load_history_page(0, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_diff_fullscreen(&mut self, cx: &mut Context<Self>) {
        self.diff_fullscreen = !self.diff_fullscreen;
        cx.notify();
    }

    pub(super) fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.is_some() {
            self.error = None;
            cx.notify();
        }
    }

    pub(super) fn set_history_commits(&mut self, commits: Vec<Commit>) -> bool {
        let retained = super::history_retention::replace(commits);
        self.history_retained_bytes = retained.retained_bytes;
        self.history_limit_reached = retained.limit_reached;
        self.history_graph_state = Default::default();
        self.history_graph_rows =
            std::rc::Rc::new(self.history_graph_state.append(&retained.commits));
        self.history_commits = std::rc::Rc::new(retained.commits);
        self.history_limit_reached
    }

    pub(super) fn append_history_commits(&mut self, commits: Vec<Commit>) -> bool {
        let previous_len = self.history_commits.len();
        let retained = super::history_retention::append(
            &self.history_commits,
            self.history_retained_bytes,
            commits,
        );
        self.history_retained_bytes = retained.retained_bytes;
        self.history_limit_reached = retained.limit_reached;
        let new_rows = self
            .history_graph_state
            .append(&retained.commits[previous_len..]);
        std::rc::Rc::make_mut(&mut self.history_graph_rows).extend(new_rows);
        self.history_commits = std::rc::Rc::new(retained.commits);
        self.history_limit_reached
    }

    /// 串行化 Git 写操作，避免争抢 `index.lock`。
    pub(super) fn begin_op(&mut self, label: &'static str, cx: &mut Context<Self>) -> bool {
        if self.busy {
            self.notify_warning(
                format!(
                    "已有 Git 操作正在执行（{}），请等待完成",
                    self.busy_label.unwrap_or("处理中…")
                ),
                cx,
            );
            return false;
        }
        self.busy = true;
        // 写操作结束后的状态才是权威结果。
        self.status_request_seq = self.status_request_seq.wrapping_add(1);
        self.workspace_refresh_pending = Default::default();
        self.busy_label = Some(label);
        self.error = None;
        cx.notify();
        true
    }

    pub(super) fn notify_success(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.pending_notification = Some(
            gpui_component::notification::Notification::success(message.into()).autohide(true),
        );
        cx.notify();
    }

    /// 暂停或冲突使用警告通知，不显示错误横幅。
    pub(super) fn notify_warning(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.pending_notification = Some(
            gpui_component::notification::Notification::warning(message.into()).autohide(true),
        );
        cx.notify();
    }

    /// 切换文件时清理 blame 并失效旧请求。
    pub(super) fn reset_blame_context(&mut self) {
        self.blame_request_seq = self.blame_request_seq.wrapping_add(1);
        self.inline_blame_request_seq = self.inline_blame_request_seq.wrapping_add(1);
        self.blame_lines = std::rc::Rc::new(Vec::new());
        self.loading_blame = false;
        self.showing_blame = false;
        self.inline_blame_text = None;
    }

    /// 切换不同上下文行数时清缓存重拉。
    pub(super) fn set_diff_view_mode(&mut self, mode: DiffViewMode, cx: &mut Context<Self>) {
        if self.diff_view_mode == mode {
            return;
        }
        let need_refetch = self.diff_view_mode.context_lines() != mode.context_lines();
        self.diff_view_mode = mode;
        if need_refetch {
            self.invalidate_active_diff_and_refetch(cx);
        } else {
            cx.notify();
        }
    }

    fn invalidate_active_diff_and_refetch(&mut self, cx: &mut Context<Self>) {
        let active_tab = self
            .active_file_tab_idx
            .and_then(|idx| self.file_tabs.get(idx))
            .cloned();
        if let Some(idx) = self.active_file_tab_idx
            && let Some(tab) = self.file_tabs.get_mut(idx)
        {
            tab.cached_diff = None;
            tab.cached_diff_syntax = None;
        }
        self.current_diff = None;
        self.current_diff_syntax = None;
        match active_tab.map(|tab| (tab.path, tab.source)) {
            Some((path, FileTabSource::Changes(kind))) => self.select_file(path, kind, cx),
            Some((path, FileTabSource::Commit { commit_id, .. })) => {
                self.select_commit_file(path, commit_id, cx);
            }
            _ => cx.notify(),
        }
    }
}
