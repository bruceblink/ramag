//! VcsView：Git 主视图。状态 + Render + active_view 路由

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

use super::commit_detail::CommitFilesRowsCacheEntry;
use super::helpers::{
    ActiveView, DiffViewMode, FileContentSnapshot, FileTab, FileTabSource, FilesViewMode,
    GroupKind, ViewMode,
};
use super::history_panel::HistoryLeftRowsCacheEntry;
use super::project_files::{ProjectRowsCacheEntry, ProjectStatusCacheEntry};
use super::reflog_view::ReflogRowsCacheEntry;
use super::repo_list::RepoListRowsCacheEntry;
use super::workspace_panel::WorkspaceRowsCacheEntry;

/// 仓库 tab UI 状态快照：文件 tabs + commit 草稿（按仓库隔离，避免串扰）
/// commit 文本通过 `pending_commit_text` + Render 内 `cx.defer_in` 写回 InputState
#[derive(Clone, Default)]
pub(super) struct RepoSessionState {
    pub file_tabs: Vec<FileTab>,
    pub active_file_tab_idx: Option<usize>,
    pub commit_text: SharedString,
    pub commit_amend: bool,
    pub commit_sign: bool,
}

#[derive(Debug, Clone)]
pub enum VcsEvent {
    /// 预留：未来从 home 跳转打开特定仓库时用
    OpenRepo(PathBuf),
}

/// 主视图状态。字段标 `pub(super)` 让兄弟子模块跨 mod 访问。
pub struct VcsView {
    pub(super) driver: Arc<dyn GitDriver>,
    /// 持久化层（recent_repos 跨重启保留）；按 RepoId 单条 CRUD（redb `repos` 表）
    pub(super) storage: Arc<dyn Storage>,
    /// 当前已打开的仓库（None = 还没选）
    pub(super) repo: Option<RepoConfig>,
    /// 工作区状态快照
    pub(super) status: Option<WorkingTreeStatus>,
    /// status / branch 静默刷新代际号，防窗口激活与文件监听的旧回包倒灌
    pub(super) status_request_seq: u64,
    /// status + branches 联合静默刷新最多一组在途；期间的新请求合并为一次补刷新。
    pub(super) workspace_refresh_in_flight: bool,
    pub(super) workspace_refresh_pending: bool,
    /// 本地分支列表
    pub(super) local_branches: Vec<Branch>,
    /// 远程分支列表
    pub(super) remote_branches: Vec<Branch>,
    /// 错误信息（打开 / 查询失败时显示）
    pub(super) error: Option<String>,
    /// 是否正在加载（点选目录后 → 各 RPC 完成前）
    pub(super) loading: bool,
    /// 整屏加载时显示的任务说明（如「正在 Clone xxx…」）；None 用通用「加载中…」
    pub(super) loading_label: Option<String>,
    /// Clone 进行中的取消位（None = 无 clone 进行中）；置位后 infra watcher kill 子进程
    pub(super) clone_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Clone 实时进度槽（git --progress stderr 最新行），loading 屏每帧读取展示
    pub(super) clone_progress: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// Clone 取消后残留的半成品目录：render 消费弹确认（删除 / 保留由用户定）
    pub(super) pending_clone_cleanup: Option<std::path::PathBuf>,
    /// 写操作正在进行中（stage / unstage / discard / commit）：避免重复点击
    pub(super) busy: bool,
    /// busy 时工具栏 spinner 旁的操作名（"Pull 中…"等）；None = 不显示指示器
    pub(super) busy_label: Option<&'static str>,
    /// 远端操作（Fetch / Pull / Push）进行中的取消位（None = 无）；置位后 infra watcher kill 子进程
    pub(super) remote_op_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// 远端操作实时进度槽（git --progress stderr 最新行），工具栏每帧读取展示
    pub(super) remote_op_progress: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// 异步操作完成后挂起的 toast；Render 持有 Window 时统一 push（与 dbclient 同模式）
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 上一次观察到的窗口激活态：仅在「未激活 → 激活」边缘触发工作区自动刷新
    pub(super) was_window_active: bool,
    /// commit message 输入框（多行）
    pub(super) commit_input: Entity<InputState>,
    /// 是否 amend 上一次提交（默认 false）
    pub(super) commit_amend: bool,
    /// 是否 GPG 签名 commit（默认 false；用户切 toggle 后保持状态）
    pub(super) commit_sign: bool,
    /// 切仓库后待恢复的 commit 草稿；Render 内 cx.defer_in 调 set_value 写回 InputState
    pub(super) pending_commit_text: Option<SharedString>,
    /// 提交草稿防抖持久化代际号：输入触发 +1，切仓 / 换代后在途写任务作废
    pub(super) commit_draft_gen: Arc<std::sync::atomic::AtomicU64>,
    /// 草稿写入串行化；等待锁后复核代际，防较慢旧写覆盖最新内容。
    pub(super) commit_draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 草稿超限或落盘失败提示；成功保存后自动清除。
    pub(super) commit_draft_error: Option<String>,
    /// 切仓库后待清空的搜索框（文件搜索 / 历史搜索）；同 pending_commit_text 的 defer 模式
    pub(super) pending_clear_search_inputs: bool,
    /// 分支 / tag 创建成功后清空输入，避免下一次误用旧名称
    pub(super) pending_clear_creation_inputs: bool,
    /// 当前选中查看 diff 的文件（path + 来源分组）
    pub(super) selected_file: Option<(String, GroupKind)>,
    /// 当前文件的 diff 快照（Rc：渲染层多列表零拷贝共享，不每帧 clone 全量 diff）
    pub(super) current_diff: Option<std::rc::Rc<FileDiff>>,
    /// diff 是否正在拉取中
    pub(super) loading_diff: bool,
    /// diff 请求代际号：快速切文件 / 切选项时，旧回包不得覆盖当前视图
    pub(super) diff_request_seq: u64,
    /// 视图模式：工作区 / 历史
    pub(super) view_mode: ViewMode,
    /// History 累积的 commit 列表（按页 append）。Rc 供 uniform_list 闭包零拷贝共享，
    /// 一律经 set_history_commits 写入（同步维护 lane 预计算）
    pub(super) history_commits: std::rc::Rc<Vec<std::rc::Rc<Commit>>>,
    /// history_commits 当前估算的堆负载，分页追加时 O(1) 延续预算。
    pub(super) history_retained_bytes: usize,
    /// 达到条数或正文内存上限；停止自动翻页并提示用搜索缩小范围。
    pub(super) history_limit_reached: bool,
    /// history_commits 的 lane 图预计算（render 直接用，不每帧重算）
    pub(super) history_graph_rows: std::rc::Rc<Vec<super::commit_graph::CommitGraphRow>>,
    /// History 是否还可能有下一页（上次拉满 PAGE_SIZE 即认为有）
    pub(super) history_has_more: bool,
    /// history 请求代际号：换搜索/切仓/刷新自增，旧回包据此丢弃（防乱序覆盖）
    pub(super) history_request_seq: u64,
    /// History 是否正在拉取中
    pub(super) loading_history: bool,
    /// Stash 列表
    pub(super) stashes: Vec<Stash>,
    /// Stash 是否正在拉取中
    pub(super) loading_stashes: bool,
    pub(super) stash_request_seq: u64,
    /// 新建分支输入框
    pub(super) create_branch_input: Entity<InputState>,
    /// 新建分支的源（None=当前 HEAD；Some(name)=指定分支作 base）
    pub(super) create_branch_base: Option<String>,
    /// Tag 列表（按 git for-each-ref 顺序）
    pub(super) tags: Vec<Tag>,
    /// Tag 是否正在拉取
    pub(super) loading_tags: bool,
    pub(super) tag_request_seq: u64,
    /// 新建 tag 输入框：tag 名
    pub(super) create_tag_input: Entity<InputState>,
    /// 新建 tag 输入框：message（可选；非空 → annotated tag，空 → lightweight）
    pub(super) create_tag_message_input: Entity<InputState>,
    /// sidebar 「本地分支」段是否折叠（默认展开）
    pub(super) collapsed_local: bool,
    /// sidebar 「远程分支」段是否折叠（默认折叠，远程通常较多）
    pub(super) collapsed_remote: bool,
    /// sidebar 「Tag」段是否折叠（默认折叠，tag 通常较多）
    pub(super) collapsed_tag: bool,
    /// sidebar 「远程仓库」段是否折叠（默认折叠；与「远程分支」段独立）
    pub(super) collapsed_remote_repos: bool,
    /// History 左栏派生行缓存，避免普通重渲染重复克隆分支、Tag 与远程仓库。
    pub(super) history_left_rows_cache: RefCell<Option<HistoryLeftRowsCacheEntry>>,
    /// 用户已点击展开的 diff spacer：(hunk_idx, run_start_line_idx)；切换文件 / commit 时清空
    pub(super) expanded_diff_spacers: std::collections::HashSet<(usize, usize)>,
    /// 远程仓库列表（git remote -v 解析）
    pub(super) remotes: Vec<Remote>,
    /// remote 列表是否正在拉取
    pub(super) loading_remotes: bool,
    pub(super) remotes_request_seq: u64,
    /// 新建远程输入框：远程名
    pub(super) create_remote_name_input: Entity<InputState>,
    /// 新建远程输入框：远程 URL
    pub(super) create_remote_url_input: Entity<InputState>,
    /// 当前在 commit 详情视图查看的 commit（None = 处于 history 列表态）
    pub(super) viewing_commit: Option<std::rc::Rc<Commit>>,
    /// 详情视图的文件列表（git diff-tree --name-status 解析）
    pub(super) commit_files: std::rc::Rc<Vec<FileStatus>>,
    /// 详情视图当前选中查看 diff 的文件
    pub(super) selected_commit_file: Option<String>,
    /// 详情视图当前文件的 diff 快照（Rc 同 current_diff）
    pub(super) commit_file_diff: Option<std::rc::Rc<FileDiff>>,
    /// 详情视图文件列表是否正在拉取
    pub(super) loading_commit_files: bool,
    /// commit 详情请求代际号：快速点不同 commit 时丢弃旧文件列表
    pub(super) commit_detail_request_seq: u64,
    /// commit 详情 / Changes 文件树折叠目录（分开维护：commit 切换时只清前者）
    pub(super) commit_files_collapsed: std::collections::HashSet<String>,
    pub(super) commit_files_collapsed_version: u64,
    pub(super) commit_files_rows_cache: RefCell<Option<CommitFilesRowsCacheEntry>>,
    pub(super) changes_collapsed_dirs: std::collections::HashSet<String>,
    /// Changes 扁平行缓存的折叠代次；目录切换或切仓时递增。
    pub(super) changes_collapsed_dirs_version: u64,
    /// Changes 派生行缓存，避免普通重渲染重复克隆全部状态并重建目录树。
    pub(super) changes_rows_cache: RefCell<Option<WorkspaceRowsCacheEntry>>,
    /// 单文件历史过滤路径（None = 全仓库 history；Some(path) = 仅该文件）
    pub(super) history_path_filter: Option<String>,
    /// commit 搜索关键词（按 message grep / author / since 解析）
    pub(super) history_search_input: Entity<InputState>,
    /// blame 行列表（当前 selected_file 的）
    /// blame 数据。Rc 供 diff 中间列 processor 零拷贝共享（大文件 blame 不每帧全量 clone）
    pub(super) blame_lines: std::rc::Rc<Vec<ramag_domain::entities::BlameLine>>,
    pub(super) loading_blame: bool,
    /// 完整 blame / 行级 blame 分开计代，互不干扰且都能取消旧回包
    pub(super) blame_request_seq: u64,
    pub(super) inline_blame_request_seq: u64,
    /// diff header 切换：false=显示 diff（默认）/ true=显示 blame
    pub(super) showing_blame: bool,
    /// 行号 inline blame：Some = 顶部 banner 显示该行作者；点行号触发，× 关闭
    pub(super) inline_blame_text: Option<SharedString>,
    /// diff 忽略空白，对应 git diff -w
    pub(super) diff_ignore_whitespace: bool,
    /// diff 视图模式：标准（默认带 3 行上下文）/ 全文件 / 仅变更（前端过滤 Context）
    pub(super) diff_view_mode: DiffViewMode,
    /// reflog 条目列表
    pub(super) reflog_entries: std::rc::Rc<Vec<ramag_domain::entities::ReflogEntry>>,
    /// Reflog 搜索结果索引缓存，避免普通重渲染重复小写、过滤并克隆全部条目。
    pub(super) reflog_rows_cache: RefCell<Option<ReflogRowsCacheEntry>>,
    /// reflog 是否正在拉取
    pub(super) loading_reflog: bool,
    pub(super) reflog_request_seq: u64,
    /// history 顶部切换：false=commit 列表（默认）/ true=reflog 列表
    pub(super) showing_reflog: bool,
    /// IDE 布局：上半区左右拖拽（左 files / 右 main）
    pub(super) ide_left_resize: Entity<ResizableState>,
    /// IDE 布局：上半 / 下半（history pane）之间的垂直拖拽
    pub(super) ide_files_resize: Entity<ResizableState>,
    /// IDE 布局：下半 history pane 右半内部 middle / commit detail 拖拽
    pub(super) detail_resize: Entity<ResizableState>,
    /// 顶层视图：仓库管理页 / 进入了仓库的 session
    pub(super) active_view: ActiveView,
    /// 最近打开仓库（启动从 storage.list_repos 加载，打开/删除时单条 upsert/delete）
    pub(super) recent_repos: std::rc::Rc<Vec<RepoConfig>>,
    /// 仓库管理页过滤 / 排序索引缓存，普通重渲染不重复处理整表。
    pub(super) repo_list_rows_cache: RefCell<Option<RepoListRowsCacheEntry>>,
    /// 仓库管理页搜索框
    pub(super) repo_search_input: Entity<InputState>,
    /// IDE 左侧 Files panel 当前显示模式（Changes / Project / Stash）
    pub(super) files_view_mode: FilesViewMode,
    /// IDE 左侧 Files panel 文件搜索框（按 path substring 过滤当前 mode 列表）
    pub(super) files_search_input: Entity<InputState>,
    /// Project Files 视图：仓库内所有 tracked + untracked 但未 ignore 的相对路径（按字母排序）
    pub(super) project_files: Vec<String>,
    /// Project Files 是否正在拉取
    pub(super) loading_project_files: bool,
    pub(super) project_files_request_seq: u64,
    /// Project Files 已展开目录（相对路径），默认空集合 = 全部折叠
    pub(super) project_expanded_dirs: std::collections::HashSet<String>,
    /// 缓存版本号：reload / toggle / expand_all / collapse_all 时递增对应字段；
    /// render 用 (files_version, expanded_version, query) 比对 cache 命中，
    /// 命中即跳过 build_tree + flatten，从 O(N log N) 降到 Rc clone
    pub(super) project_files_version: u64,
    pub(super) project_expanded_dirs_version: u64,
    pub(super) project_rows_cache: RefCell<Option<ProjectRowsCacheEntry>>,
    pub(super) project_status_cache: RefCell<Option<ProjectStatusCacheEntry>>,
    /// Project Files 虚拟列表滚动句柄（uniform_list 行级虚拟化，与 dbclient 表树同款）
    pub(super) project_scroll: UniformListScrollHandle,
    /// Project Files 模式当前选中查看内容的文件路径（与 selected_file 互独立：
    /// 前者展示**文件内容**，后者展示 diff，避免两个视图模式互相干扰）
    pub(super) selected_pf_path: Option<String>,
    /// Project Files 当前选中文件的内容快照（None = 未加载 / 未选中）
    pub(super) current_file_content: Option<FileContentSnapshot>,
    /// 文件内容是否正在读盘
    pub(super) loading_file_content: bool,
    /// Project 文件内容请求代际号：同一路径重复打开时旧读盘结果不得覆盖新结果
    pub(super) file_content_request_seq: u64,
    /// Project Files 文件内容渲染的虚拟列表滚动句柄（垂直方向，uniform_list 行级虚拟化）
    pub(super) pf_content_scroll: UniformListScrollHandle,
    /// Diff 视图的虚拟化列表滚动 handle（unified / split 共用一个）
    pub(super) diff_scroll: UniformListScrollHandle,
    /// commit 文件列表 / 冲突编辑器滚动
    pub(super) commit_files_scroll: UniformListScrollHandle,
    /// Changes 变更文件树 uniform_list 滚动句柄（4 组扁平为单列表 + 分组表头行）
    pub(super) changes_scroll: UniformListScrollHandle,
    /// history 左栏（本地/远程分支 + tag 合并为单 uniform_list）滚动句柄
    pub(super) history_left_scroll: UniformListScrollHandle,
    pub(super) conflict_ours_scroll: UniformListScrollHandle,
    pub(super) conflict_theirs_scroll: UniformListScrollHandle,
    /// 虚拟列表滚动句柄：history 中栏 + reflog 列表（uniform_list 行级，万级也不卡）
    pub(super) history_scroll: UniformListScrollHandle,
    pub(super) reflog_scroll: UniformListScrollHandle,
    pub(super) stash_scroll: UniformListScrollHandle,
    pub(super) rebase_scroll: UniformListScrollHandle,
    /// pf_content / diff 横向滚动句柄：uniform_list 管 Y，外层 overflow_x_scroll 管 X
    pub(super) pf_content_h_scroll: ScrollHandle,
    /// diff 横滚 handle（unified 单栏 + split 左右两栏共享，两栏一起横滚）
    pub(super) diff_h_scroll: ScrollHandle,
    /// 下半区 history pane 是否显示（默认隐藏，工具栏 PanelBottom 图标 toggle）
    pub(super) history_pane_visible: bool,

    // ---- 多仓库 Tabs ----
    pub(super) open_repos: Vec<RepoConfig>,
    /// 启动标签恢复只在用户尚未手动操作仓库时生效。
    pub(super) startup_repo_restore_allowed: bool,
    /// 仓库 Tab Bar 横向滚动句柄：仓库开多了超出宽度时可横滚，固定「仓库管理」tab 常驻不参与滚动
    pub(super) repos_scroll: ScrollHandle,
    pub(super) file_tabs: Vec<FileTab>,
    pub(super) active_file_tab_idx: Option<usize>,
    pub(super) repo_session_cache: std::collections::HashMap<String, RepoSessionState>,
    pub(super) repo_session_order: std::collections::VecDeque<String>,

    // ---- Clone 对话框 ----
    pub(super) clone_url_input: Entity<InputState>,
    pub(super) clone_dest_path: Option<PathBuf>,
    pub(super) show_clone_panel: bool,

    // ---- Interactive Rebase ----
    pub(super) show_rebase_plan: bool,
    pub(super) rebase_plan_onto: String,
    pub(super) rebase_todos: Vec<RebaseTodo>,
    pub(super) loading_rebase_plan: bool,
    pub(super) rebase_request_seq: u64,

    // ---- Conflict Editor ----
    pub(super) conflict_editor_path: Option<String>,
    pub(super) conflict_content: Option<std::rc::Rc<ConflictContent>>,
    pub(super) loading_conflict: bool,
    /// 冲突内容请求代际号：快速切冲突文件时只接收最后一次请求
    pub(super) conflict_request_seq: u64,

    /// 当前仓库的文件系统监听句柄（drop 即停）；切仓重建，关仓置 None
    pub(super) fs_watcher: Option<crate::watcher::RepoWatcher>,

    /// 视图焦点（cmd-w / 全局 action dispatch）
    pub(super) focus_handle: FocusHandle,
}

impl EventEmitter<VcsEvent> for VcsView {}

impl Focusable for VcsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl VcsView {
    /// 异步回调归属校验：发起请求时捕获的 `repo_id` 与当前打开仓库不一致（用户已切仓）时返回 false。
    /// 回调应在重置 loading/busy 标志后、写入派生数据前调用，不匹配则丢弃旧仓库结果，避免切仓串味。
    pub(super) fn is_current_repo(&self, repo_id: &RepoId) -> bool {
        self.repo.as_ref().map(|r| &r.id) == Some(repo_id)
    }

    /// 需要干净 Git 操作状态的入口统一调用；冲突处理的 stage/continue/abort 不走此闸门。
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

    /// 工作区是否有未提交改动（含 untracked）：checkout 前的 dirty 判断
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

    /// 切换 IDE 左侧 Files panel 的视图模式（Changes / Project / Stash）
    ///
    /// 切到 Project 模式时若列表还没加载，触发一次异步拉取
    pub(super) fn set_files_view_mode(&mut self, mode: FilesViewMode, cx: &mut Context<Self>) {
        if self.files_view_mode != mode {
            self.files_view_mode = mode;
            // 切 mode 时清掉「另一边」的选中态，避免主区残留旧视图
            // - 离开 Project：清 selected_pf_path / current_file_content
            // - 离开 Changes：清 selected_file / current_diff
            if !matches!(mode, FilesViewMode::Project) {
                self.selected_pf_path = None;
                self.current_file_content = None;
                self.loading_file_content = false;
                self.project_rows_cache.get_mut().take();
                self.project_status_cache.get_mut().take();
            } else {
                self.selected_file = None;
                self.current_diff = None;
                self.loading_diff = false;
            }
            if !matches!(mode, FilesViewMode::Changes) {
                self.changes_rows_cache.get_mut().take();
            }
            cx.notify();
            // 切到任何 mode 都立即异步 reload 对应数据（实时更新，不需要刷新按钮）
            // - Changes: status；Project: ls-files；Stash: stash list；Branches: 分支列表
            self.refresh_current_files_view(cx);
        }
    }

    /// 切到仓库管理页（保留当前 repo 数据，仅切视图）
    pub(super) fn show_repo_list(&mut self, cx: &mut Context<Self>) {
        self.active_view = ActiveView::RepoList;
        cx.notify();
    }

    /// 清空所有 session 派生数据（diff / pf 内容 / commit 详情 / 历史 / 文件 tabs 等）
    /// open_repo_async 里切仓库时调用，确保新仓库不残留旧仓库的视图状态
    pub(super) fn clear_session_data(&mut self) {
        self.selected_file = None;
        self.current_diff = None;
        self.loading_diff = false;
        self.diff_request_seq = self.diff_request_seq.wrapping_add(1);
        self.selected_pf_path = None;
        self.current_file_content = None;
        self.loading_file_content = false;
        self.file_content_request_seq = self.file_content_request_seq.wrapping_add(1);
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
        // 代际推进：上一个仓库在途的 history 回包全部失效
        self.history_request_seq = self.history_request_seq.wrapping_add(1);
        self.project_files.clear();
        self.project_status_cache.get_mut().take();
        self.loading_project_files = false;
        self.project_files_request_seq = self.project_files_request_seq.wrapping_add(1);
        self.project_expanded_dirs.clear();
        self.file_tabs.clear();
        self.active_file_tab_idx = None;
        // 以下均为仓库级状态，跨仓残留会"串味"：
        // 单文件历史过滤 / reflog / blame / 展开的 diff spacer / 上一仓的错误横幅
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
        // 列表清空：切仓后 open_repo_async 会重拉，避免拉取期间短暂显示旧仓数据
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
        self.workspace_refresh_pending = false;
        // 搜索框内容属仓库上下文，经 Render 的 defer 写回清空（异步处拿不到 Window）
        self.pending_clear_search_inputs = true;
        self.pending_clear_creation_inputs = true;
        // 注：busy 不清——它是全局写操作闸，进行中的 op 结束时自会复位；
        // diff_ignore_whitespace / diff_view_mode 是用户偏好，跨仓保留
    }

    /// 首次打开 lazy load 首页 commits，避免仓库打开就预拉 git log
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

    /// 清除当前错误（关闭顶部错误 banner 时调用）
    pub(super) fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.is_some() {
            self.error = None;
            cx.notify();
        }
    }

    /// 挂起一条成功 toast，待 Render 持有 Window 时推送（异步回调里拿不到 Window）
    /// history 列表统一写入口：同步重算 lane 图缓存，render 零重算零全量拷贝
    pub(super) fn set_history_commits(&mut self, commits: Vec<Commit>) -> bool {
        let retained = super::history_retention::replace(commits);
        self.history_retained_bytes = retained.retained_bytes;
        self.history_limit_reached = retained.limit_reached;
        self.history_graph_rows =
            std::rc::Rc::new(super::commit_graph::build_commit_lanes(&retained.commits));
        self.history_commits = std::rc::Rc::new(retained.commits);
        self.history_limit_reached
    }

    /// 分页追加只复制 `Rc` 指针；旧提交的 subject/body 不再随页数反复深拷贝。
    pub(super) fn append_history_commits(&mut self, commits: Vec<Commit>) -> bool {
        let retained = super::history_retention::append(
            &self.history_commits,
            self.history_retained_bytes,
            commits,
        );
        self.history_retained_bytes = retained.retained_bytes;
        self.history_limit_reached = retained.limit_reached;
        self.history_graph_rows =
            std::rc::Rc::new(super::commit_graph::build_commit_lanes(&retained.commits));
        self.history_commits = std::rc::Rc::new(retained.commits);
        self.history_limit_reached
    }

    /// 写操作统一入口闸：已有操作进行中直接拒绝（防止并发 git 写争抢 index.lock），
    /// 否则置忙碌态 + 进度文案并清上次错误。调用方拿到 false 应立即 return
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
        // 所有更早发起的静默 status 刷新失效；写操作结束时取得的状态才是权威结果。
        self.status_request_seq = self.status_request_seq.wrapping_add(1);
        self.workspace_refresh_pending = false;
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

    /// 预期中的暂停 / 冲突用 warning toast，不与真正失败共用红色错误横幅。
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

    /// 文件上下文变化时清理 blame，并让尚未返回的旧请求失效。
    pub(super) fn reset_blame_context(&mut self) {
        self.blame_request_seq = self.blame_request_seq.wrapping_add(1);
        self.inline_blame_request_seq = self.inline_blame_request_seq.wrapping_add(1);
        self.blame_lines = std::rc::Rc::new(Vec::new());
        self.loading_blame = false;
        self.showing_blame = false;
        self.inline_blame_text = None;
    }

    /// 切换 diff 视图模式；FullFile 与 Standard 后端 unified 行数不同，要清缓存重拉
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

    /// 切换 `git diff -w`；后端结果不同，当前 diff 必须失效并重拉。
    pub(super) fn toggle_diff_ignore_whitespace(&mut self, cx: &mut Context<Self>) {
        self.diff_ignore_whitespace = !self.diff_ignore_whitespace;
        self.invalidate_active_diff_and_refetch(cx);
    }

    /// 清当前 active tab 的 diff 缓存 + 触发重拉（视 source 调对应 select_*）
    fn invalidate_active_diff_and_refetch(&mut self, cx: &mut Context<Self>) {
        let active_tab = self
            .active_file_tab_idx
            .and_then(|idx| self.file_tabs.get(idx))
            .cloned();
        if let Some(idx) = self.active_file_tab_idx
            && let Some(tab) = self.file_tabs.get_mut(idx)
        {
            tab.cached_diff = None;
        }
        self.current_diff = None;
        match active_tab.map(|tab| (tab.path, tab.source)) {
            Some((path, FileTabSource::Changes(kind))) => self.select_file(path, kind, cx),
            Some((path, FileTabSource::Commit { commit_id, .. })) => {
                self.select_commit_file(path, commit_id, cx);
            }
            _ => cx.notify(),
        }
    }
}
