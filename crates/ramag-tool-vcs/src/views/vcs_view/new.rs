//! VcsView::new：所有 InputState / Resizable / Scroll handle 字段初始化

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{AppContext as _, Context, ScrollHandle, UniformListScrollHandle, Window};
use gpui_component::{
    input::{InputEvent, InputState},
    notification::Notification,
    resizable::ResizableState,
};
use ramag_domain::entities::{
    MAX_COMMIT_MESSAGE_BYTES, MAX_GIT_NAME_ARG_BYTES, MAX_GIT_POSITIONAL_ARG_BYTES,
    MAX_GIT_TAG_MESSAGE_BYTES,
};
use ramag_domain::traits::{GitDriver, Storage};

use super::super::helpers::{ActiveView, DiffViewMode, FilesViewMode, ViewMode};
use super::VcsView;

fn bounded_input(
    max_bytes: usize,
    window: &mut Window,
    cx: &mut Context<InputState>,
) -> InputState {
    InputState::new(window, cx).validate(move |value, _| value.len() <= max_bytes)
}

impl VcsView {
    pub fn new(
        driver: Arc<dyn GitDriver>,
        storage: Arc<dyn Storage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit_input = cx.new(|cx_inner| {
            bounded_input(MAX_COMMIT_MESSAGE_BYTES, window, cx_inner)
                .multi_line(true)
                .rows(3)
                .placeholder("commit message（首行 subject，空行后写 body）")
        });
        let create_branch_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_NAME_ARG_BYTES, window, cx_inner)
                .placeholder("新分支名（基于当前 HEAD）")
        });
        let create_tag_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_NAME_ARG_BYTES, window, cx_inner).placeholder("tag 名")
        });
        let create_tag_message_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_TAG_MESSAGE_BYTES, window, cx_inner).placeholder("备注（可选）")
        });
        let create_remote_name_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_NAME_ARG_BYTES, window, cx_inner)
                .placeholder("远程名（如 origin）")
        });
        let create_remote_url_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_POSITIONAL_ARG_BYTES, window, cx_inner)
                .placeholder("远程 URL（HTTPS / SSH）")
        });
        let history_search_input = cx.new(|cx_inner| {
            ramag_ui::bounded_search_input(window, cx_inner)
                .placeholder("搜索：关键词 / @作者 / 7d/1m 时间下限")
        });
        let clone_url_input = cx.new(|cx_inner| {
            bounded_input(MAX_GIT_POSITIONAL_ARG_BYTES, window, cx_inner)
                .placeholder("仓库 URL（HTTPS / SSH）")
        });
        let ide_left_resize = cx.new(|_| ResizableState::default());
        let ide_files_resize = cx.new(|_| ResizableState::default());
        let detail_resize = cx.new(|_| ResizableState::default());
        // 主分隔宽高跨重启（左栏宽 / 上下分栏高）
        ramag_ui::persist_resizable_sizes(&ide_left_resize, "split_vcs_left", window, cx).detach();
        ramag_ui::persist_resizable_sizes(&ide_files_resize, "split_vcs_main", window, cx).detach();
        ramag_ui::persist_resizable_sizes(&detail_resize, "split_vcs_detail", window, cx).detach();
        let repo_search_input = cx.new(|cx_inner| {
            ramag_ui::bounded_search_input(window, cx_inner).placeholder("搜索仓库（名称 / 路径）")
        });
        let files_search_input = cx.new(|cx_inner| {
            ramag_ui::bounded_search_input(window, cx_inner).placeholder("搜索文件路径")
        });
        // 提交草稿输入即防抖持久化（重启后可恢复；切仓恢复走 session cache）。
        // 恢复写回用 set_value（不发 Change），不会触发本订阅形成回写环
        let commit_input_for_sub = commit_input.clone();
        cx.subscribe_in(
            &commit_input,
            window,
            move |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    if ramag_ui::clamp_multiline_input_value(
                        &commit_input_for_sub,
                        MAX_COMMIT_MESSAGE_BYTES,
                        window,
                        cx,
                    ) {
                        this.pending_notification = Some(
                            Notification::warning(format!(
                                "提交信息最多保留 {} MiB，超出部分已截断",
                                MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
                            ))
                            .autohide(true),
                        );
                    }
                    this.schedule_commit_draft_persist(cx);
                }
            },
        )
        .detach();
        // 订阅搜索框 Change → notify 主 view 重渲染（触发文件过滤即时反馈）
        cx.subscribe(
            &files_search_input,
            |_this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        )
        .detach();
        // 仓库搜索同样即时过滤（此前缺订阅，输入不触发重渲染）
        cx.subscribe(
            &repo_search_input,
            |_this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        )
        .detach();
        // 历史搜索框：reflog 模式为客户端即时过滤，需要输入即重渲染
        // （commit 模式输入只重渲染无副作用，git 侧搜索仍靠显式应用）
        cx.subscribe(
            &history_search_input,
            |this: &mut Self, _, event: &InputEvent, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } if !this.showing_reflog => {
                    this.apply_history_search(cx);
                }
                _ => {}
            },
        )
        .detach();
        // 用户在外部（编辑器 / 终端）改动文件后切回窗口 → 自动刷新工作区，
        // 不必手动点刷新。仅在「未激活 → 激活」边缘且已打开仓库时触发
        cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            let active = window.is_window_active();
            let became_active = active && !this.was_window_active;
            this.was_window_active = active;
            if became_active && this.repo.is_some() && !this.loading {
                this.refresh_workspace_silent(cx);
            }
        })
        .detach();
        let this = Self {
            driver,
            storage,
            repo: None,
            status: None,
            status_request_seq: 0,
            workspace_refresh_in_flight: false,
            workspace_refresh_pending: false,
            local_branches: Vec::new(),
            remote_branches: Vec::new(),
            error: None,
            loading: false,
            loading_label: None,
            clone_cancel: None,
            clone_progress: None,
            pending_clone_cleanup: None,
            busy: false,
            busy_label: None,
            remote_op_cancel: None,
            remote_op_progress: None,
            pending_notification: None,
            was_window_active: window.is_window_active(),
            commit_input,
            commit_amend: false,
            commit_sign: false,
            pending_commit_text: None,
            commit_draft_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            commit_draft_write_lock: Arc::new(futures::lock::Mutex::new(())),
            commit_draft_error: None,
            pending_clear_search_inputs: false,
            pending_clear_creation_inputs: false,
            selected_file: None,
            current_diff: None,
            loading_diff: false,
            diff_request_seq: 0,
            view_mode: ViewMode::Workspace,
            history_commits: std::rc::Rc::new(Vec::new()),
            history_retained_bytes: 0,
            history_limit_reached: false,
            history_graph_rows: std::rc::Rc::new(Vec::new()),
            history_has_more: false,
            history_request_seq: 0,
            loading_history: false,
            stashes: Vec::new(),
            loading_stashes: false,
            stash_request_seq: 0,
            create_branch_input,
            create_branch_base: None,
            tags: Vec::new(),
            loading_tags: false,
            tag_request_seq: 0,
            create_tag_input,
            create_tag_message_input,
            collapsed_local: false,
            collapsed_remote: true,
            collapsed_tag: true,
            collapsed_remote_repos: true,
            history_left_rows_cache: RefCell::new(None),
            expanded_diff_spacers: std::collections::HashSet::new(),
            remotes: Vec::new(),
            loading_remotes: false,
            remotes_request_seq: 0,
            create_remote_name_input,
            create_remote_url_input,
            viewing_commit: None,
            commit_files: std::rc::Rc::new(Vec::new()),
            selected_commit_file: None,
            commit_file_diff: None,
            loading_commit_files: false,
            commit_detail_request_seq: 0,
            commit_files_collapsed: std::collections::HashSet::new(),
            commit_files_collapsed_version: 0,
            commit_files_rows_cache: RefCell::new(None),
            changes_collapsed_dirs: std::collections::HashSet::new(),
            changes_collapsed_dirs_version: 0,
            changes_rows_cache: RefCell::new(None),
            history_path_filter: None,
            history_search_input,
            blame_lines: std::rc::Rc::new(Vec::new()),
            loading_blame: false,
            blame_request_seq: 0,
            inline_blame_request_seq: 0,
            showing_blame: false,
            inline_blame_text: None,
            diff_ignore_whitespace: false,
            diff_view_mode: DiffViewMode::Standard,
            reflog_entries: std::rc::Rc::new(Vec::new()),
            reflog_rows_cache: RefCell::new(None),
            loading_reflog: false,
            reflog_request_seq: 0,
            showing_reflog: false,
            ide_left_resize,
            ide_files_resize,
            detail_resize,
            active_view: ActiveView::RepoList,
            recent_repos: std::rc::Rc::new(Vec::new()),
            repo_list_rows_cache: RefCell::new(None),
            repo_search_input,
            // 默认进 Changes：打开仓库最常见的任务是看改动与提交（Project 树按需切换）
            files_view_mode: FilesViewMode::Changes,
            files_search_input,
            project_files: Vec::new(),
            loading_project_files: false,
            project_files_request_seq: 0,
            project_expanded_dirs: std::collections::HashSet::new(),
            project_files_version: 0,
            project_expanded_dirs_version: 0,
            project_rows_cache: RefCell::new(None),
            project_status_cache: RefCell::new(None),
            project_scroll: UniformListScrollHandle::new(),
            selected_pf_path: None,
            current_file_content: None,
            loading_file_content: false,
            file_content_request_seq: 0,
            pf_content_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            commit_files_scroll: UniformListScrollHandle::new(),
            changes_scroll: UniformListScrollHandle::new(),
            history_left_scroll: UniformListScrollHandle::new(),
            conflict_ours_scroll: UniformListScrollHandle::new(),
            conflict_theirs_scroll: UniformListScrollHandle::new(),
            history_scroll: UniformListScrollHandle::new(),
            reflog_scroll: UniformListScrollHandle::new(),
            stash_scroll: UniformListScrollHandle::new(),
            rebase_scroll: UniformListScrollHandle::new(),
            pf_content_h_scroll: ScrollHandle::new(),
            diff_h_scroll: ScrollHandle::new(),
            history_pane_visible: false,
            open_repos: Vec::new(),
            startup_repo_restore_allowed: true,
            repos_scroll: ScrollHandle::new(),
            file_tabs: Vec::new(),
            active_file_tab_idx: None,
            repo_session_cache: std::collections::HashMap::new(),
            repo_session_order: std::collections::VecDeque::new(),
            clone_url_input,
            clone_dest_path: None,
            show_clone_panel: false,
            show_rebase_plan: false,
            rebase_plan_onto: String::new(),
            rebase_todos: Vec::new(),
            loading_rebase_plan: false,
            rebase_request_seq: 0,
            conflict_editor_path: None,
            conflict_content: None,
            loading_conflict: false,
            conflict_request_seq: 0,
            fs_watcher: None,
            focus_handle: cx.focus_handle(),
        };
        Self::load_recent_repos_async(cx);
        this
    }
}
