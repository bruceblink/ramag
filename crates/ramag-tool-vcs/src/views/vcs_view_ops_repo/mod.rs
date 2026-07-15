//! 仓库 / Session 管理 ops：pick_directory / open_recent_repo / remove_recent_repo /
//! remove_open_repo / open_repo_async（共享异步流：open + 拉 status / 分支 / stash 等）

use gpui::Context;
use ramag_domain::entities::BranchKind;
use tracing::{error, info};

use super::helpers::{ActiveView, FileTab, FileTabSource, FilesViewMode};
use super::vcs_view::{RepoSessionState, VcsView};

/// Project Files 点击文件后读盘上限（4MB）；超过截断后 UI 显示提示
pub(super) const PF_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 单窗口打开仓库标签上限；每个标签会保留会话元数据，并可能在驱动层持有仓库句柄。
pub(super) const MAX_OPEN_REPOS: usize = 32;
/// 仅保留最近访问仓库的轻量 UI 会话，避免长时间运行后路径与草稿无限累积。
const REPO_SESSION_CACHE_LIMIT: usize = 32;

/// worker 线程跨线程返回结构（Send）；主线程 finalize 后包 Rc 成 FileContentSnapshot
pub(super) struct RawFileContent {
    pub(super) path: String,
    pub(super) lines: Vec<String>,
    pub(super) max_chars: usize,
    pub(super) truncated: bool,
    pub(super) binary: bool,
    pub(super) error: Option<String>,
}

impl RawFileContent {
    pub(in crate::views) fn with_error(path: String, error: String) -> Self {
        Self {
            path,
            lines: Vec::new(),
            max_chars: 0,
            truncated: false,
            binary: false,
            error: Some(error),
        }
    }
}

impl VcsView {
    /// 弹出系统目录选择器；用户选完后异步打开仓库
    pub(super) fn pick_directory(&mut self, cx: &mut Context<Self>) {
        let driver = self.driver.clone();
        self.loading = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dialog = rfd::FileDialog::new().set_title("选择 Git 仓库目录");
            let Some(path) = dialog.pick_folder() else {
                let _ = this.update(cx, |this, cx| {
                    this.loading = false;
                    this.loading_label = None;
                    cx.notify();
                });
                return;
            };
            open_repo_async(&this, driver, path, cx).await;
        })
        .detach();
    }

    /// 从最近列表点击仓库行 → 直接打开（不弹文件对话框）
    pub(super) fn open_recent_repo(&mut self, path: String, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，完成后再切换仓库", cx);
            return;
        }
        if !self.ensure_open_repo_capacity(&path, cx) {
            return;
        }
        let driver = self.driver.clone();
        let pb = std::path::PathBuf::from(path);
        self.loading = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            open_repo_async(&this, driver, pb, cx).await;
        })
        .detach();
    }

    /// 新仓库标签创建闸门；已打开仓库始终允许切换。
    pub(super) fn ensure_open_repo_capacity(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        if self.open_repos.iter().any(|repo| repo.path == path)
            || self.open_repos.len() < MAX_OPEN_REPOS
        {
            return true;
        }
        self.notify_warning(
            format!("仓库标签已达上限（{MAX_OPEN_REPOS} 个），请先关闭不需要的标签"),
            cx,
        );
        false
    }

    /// 从最近列表移除（不删磁盘）；按 path 找 RepoId 后调 storage.delete_repo
    pub(super) fn remove_recent_repo(&mut self, path: String, cx: &mut Context<Self>) {
        let repo_id = self
            .recent_repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.id.clone());
        std::rc::Rc::make_mut(&mut self.recent_repos).retain(|r| r.path != path);
        if let Some(id) = repo_id {
            self.delete_repo_async(id, cx);
        }
        cx.notify();
    }

    /// 刷新 Files panel 当前视图（Changes/Stash/Project 各调对应 reload）
    pub(super) fn refresh_current_files_view(&mut self, cx: &mut Context<Self>) {
        match self.files_view_mode {
            FilesViewMode::Changes => self.reload_status_silent(cx),
            FilesViewMode::Stash => self.reload_stashes(cx),
            FilesViewMode::Project => self.reload_project_files(cx),
        }
    }

    /// 异步拉 Project Files（git ls-files：tracked + 未 ignore 的 untracked）
    pub(super) fn reload_project_files(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.loading_project_files = true;
        self.project_files_request_seq = self.project_files_request_seq.wrapping_add(1);
        let request_seq = self.project_files_request_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = driver.list_files(&repo).await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.project_files_request_seq != request_seq {
                    return;
                }
                this.loading_project_files = false;
                match result {
                    Ok(mut paths) => {
                        // 字母序：让目录树渲染稳定（同一目录文件聚拢）
                        paths.sort();
                        this.project_files = paths;
                    }
                    Err(e) => {
                        error!(error = %e, "vcs: list project files failed");
                        // 失败时仍清空避免显示旧数据；错误以 banner 形式提示
                        this.project_files = Vec::new();
                        this.error = Some(format!("加载 Project Files 失败: {e}"));
                    }
                }
                // 列表内容变了 → 递增版本号让 render 缓存失效
                this.project_files_version = this.project_files_version.wrapping_add(1);
                cx.notify();
            });
        })
        .detach();
    }

    /// 点击文件复用 file_tabs：命中已开则激活，否则追加并在线程池读盘。4MB 上限 + NUL 字节判二进制
    pub(super) fn select_pf_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some((repo_path, repo_id)) = self
            .repo
            .as_ref()
            .map(|repo| (repo.path.clone(), repo.id.clone()))
        else {
            return;
        };
        let existing = self
            .file_tabs
            .iter()
            .position(|t| t.path == path && t.source == FileTabSource::ProjectFiles);
        if existing.is_none() && !self.ensure_file_tab_capacity(cx) {
            return;
        }
        // 点击 Project Files 文件 → 关掉 commit detail，避免主区残留 commit diff
        if self.viewing_commit.is_some() {
            self.commit_detail_request_seq = self.commit_detail_request_seq.wrapping_add(1);
            self.viewing_commit = None;
            self.reset_commit_files_tree();
            self.selected_commit_file = None;
            self.commit_file_diff = None;
            self.loading_commit_files = false;
        }
        self.file_content_request_seq = self.file_content_request_seq.wrapping_add(1);
        let request_seq = self.file_content_request_seq;
        if self.selected_pf_path.as_deref() != Some(path.as_str()) {
            self.reset_blame_context();
        }
        let idx = if let Some(i) = existing {
            i
        } else {
            self.file_tabs.push(FileTab {
                path: path.clone(),
                source: FileTabSource::ProjectFiles,
                cached_diff: None,
                cached_content: None,
            });
            self.file_tabs.len() - 1
        };
        self.active_file_tab_idx = Some(idx);
        let tab = self.file_tabs[idx].clone();
        self.activate_file_tab_state(tab.clone());
        cx.notify();
        if tab.cached_content.is_some() {
            return;
        }

        let repo_root = std::path::PathBuf::from(&repo_path);
        cx.spawn(async move |this, cx| {
            let path_for_worker = path.clone();
            let raw = match ramag_app::run_blocking(move || {
                Ok(read_raw_file_content(&repo_root, &path_for_worker))
            })
            .await
            {
                Ok(raw) => raw,
                Err(e) => {
                    RawFileContent::with_error(path.clone(), format!("文件读取任务失败: {e}"))
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id)
                    || this.file_content_request_seq != request_seq
                    || this.selected_pf_path.as_deref() != Some(path.as_str())
                {
                    return;
                }
                let snapshot = Some(finalize_file_snapshot(raw));
                if let Some(tab) = this
                    .file_tabs
                    .iter_mut()
                    .find(|t| t.path == path && t.source == FileTabSource::ProjectFiles)
                {
                    tab.cached_content = snapshot.clone();
                }
                this.prune_file_tab_payloads();
                if this.selected_pf_path.as_deref() == Some(path.as_str()) {
                    this.loading_file_content = false;
                    this.current_file_content = snapshot;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 静默拉一次工作区状态（不显 loading 占整屏，仅写回 self.status）
    pub(super) fn reload_status_silent(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        let driver = self.driver.clone();
        self.status_request_seq = self.status_request_seq.wrapping_add(1);
        let request_seq = self.status_request_seq;
        cx.spawn(async move |this, cx| {
            let new_status = match driver.status(&repo).await {
                Ok(s) => Some(s),
                Err(e) => {
                    // 静默刷新失败保留旧状态（仓库可能被外部删除/损坏），留日志可排查
                    tracing::warn!(error = %e, "vcs: silent status reload failed");
                    None
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo) || this.status_request_seq != request_seq {
                    return;
                }
                if let Some(s) = new_status {
                    this.status = Some(s);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭指定路径的 tab；若是当前 tab 则尝试切到下一个，否则直接移除
    pub(super) fn remove_open_repo(&mut self, path: String, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            self.notify_warning("当前操作尚未完成，完成后再关闭仓库标签", cx);
            return;
        }
        let Some(repo_id) = self
            .open_repos
            .iter()
            .find(|repo| repo.path == path)
            .map(|repo| repo.id.clone())
        else {
            self.notify_warning("仓库标签已不存在", cx);
            return;
        };
        let is_current = self.repo.as_ref().map(|r| r.path == path).unwrap_or(false);
        if is_current {
            // 关闭标签不应静默丢掉尚未提交的 message 与已打开文件；本次进程内重开可恢复。
            self.save_current_session_to_cache(cx);
        }
        self.loading = true;
        self.loading_label = Some("正在关闭仓库…".into());
        cx.notify();

        let driver = self.driver.clone();
        cx.spawn(async move |this, cx| {
            let result = driver.close_repo(&repo_id).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.loading_label = None;
                match result {
                    Ok(()) => {
                        this.open_repos.retain(|repo| repo.path != path);
                        this.persist_open_repos(cx);
                        if is_current {
                            if let Some(next) = this.open_repos.first().cloned() {
                                this.open_recent_repo(next.path, cx);
                            } else {
                                this.reset_session_state(cx);
                            }
                        } else {
                            cx.notify();
                        }
                    }
                    Err(e) => {
                        error!(error = %e, repo_id = %repo_id, "vcs: close repo failed");
                        this.error = Some(format!("关闭仓库失败：{e}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn reset_session_state(&mut self, cx: &mut Context<Self>) {
        self.fs_watcher = None;
        self.clear_session_data();
        self.repo = None;
        self.status = None;
        self.local_branches.clear();
        self.remote_branches.clear();
        self.active_view = ActiveView::RepoList;
        cx.notify();
    }

    /// 把当前仓库的文件 tab + commit 草稿状态保存到缓存（切换仓库前调用）
    ///
    /// commit_input 的当前文本同时入快照——切回该仓库时再原样恢复，避免跨仓库串扰
    pub(super) fn save_current_session_to_cache(&mut self, cx: &gpui::App) {
        let Some(path) = self.repo.as_ref().map(|r| r.path.clone()) else {
            return;
        };
        let commit_text = self.commit_input.read(cx).value();
        // 切仓即持久化当前草稿（作废在途防抖任务——其读到的将是新仓文本），重启后可恢复
        self.commit_draft_gen = self.commit_draft_gen.wrapping_add(1);
        {
            let storage = self.storage.clone();
            let key = commit_draft_pref_key(&path);
            let text = commit_text.to_string();
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = storage.set_preference(&key, &text).await {
                        tracing::warn!(error = %e, "persist commit draft on switch failed");
                    }
                })
                .detach();
        }
        // 切回仓库时本就必须重读磁盘 / Git 状态；缓存只保留 tab 元数据，不能让旧 diff
        // 或 4MB 文件快照在每个访问过的仓库中长期占用内存。
        let mut file_tabs = self.file_tabs.clone();
        strip_file_tab_payloads(&mut file_tabs);
        cache_repo_session(
            &mut self.repo_session_cache,
            &mut self.repo_session_order,
            path,
            RepoSessionState {
                file_tabs,
                active_file_tab_idx: self.active_file_tab_idx,
                commit_text,
                commit_amend: self.commit_amend,
                commit_sign: self.commit_sign,
            },
        );
    }

    /// 提交草稿防抖持久化：输入停顿 800ms 后按当前仓库 path 写 prefs；
    /// 期间再输入 / 切仓则代际不符自动作废（切仓另有同步写兜底）
    pub(super) fn schedule_commit_draft_persist(&mut self, cx: &mut gpui::Context<Self>) {
        if self.repo.is_none() {
            return;
        }
        self.commit_draft_gen = self.commit_draft_gen.wrapping_add(1);
        let generation = self.commit_draft_gen;
        let storage = self.storage.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(800))
                .await;
            // 代际校验通过才读文本与 path（保证读到的仍是同一仓库的草稿）
            let snapshot = this
                .update(cx, |this, cx| {
                    if this.commit_draft_gen != generation {
                        return None;
                    }
                    let path = this.repo.as_ref().map(|r| r.path.clone())?;
                    Some((path, this.commit_input.read(cx).value().to_string()))
                })
                .ok()
                .flatten();
            let Some((path, text)) = snapshot else {
                return;
            };
            if let Err(e) = storage
                .set_preference(&commit_draft_pref_key(&path), &text)
                .await
            {
                tracing::warn!(error = %e, "persist commit draft failed");
            }
        })
        .detach();
    }

    /// 从缓存还原文件 tab + commit 面板状态；commit 文本通过 pending_commit_text 让
    /// Render 阶段（持有 Window）写回 InputState。返回 true 表示命中缓存
    pub(super) fn restore_session_from_cache(&mut self, path: &str) -> bool {
        let cached = self.repo_session_cache.get(path).cloned();
        if cached.is_some() {
            touch_repo_session(&mut self.repo_session_order, path);
        }
        match cached {
            Some(mut state) => {
                // 切回仓库时磁盘 / HEAD 可能已被终端或其它工具修改；保留 tab，丢弃内容缓存。
                for tab in &mut state.file_tabs {
                    tab.cached_diff = None;
                    tab.cached_content = None;
                }
                self.file_tabs = state.file_tabs;
                self.active_file_tab_idx = state.active_file_tab_idx;
                self.commit_amend = state.commit_amend;
                self.commit_sign = state.commit_sign;
                // 即使文本相同也写：保证 Render 一定走 set_value 覆盖前一个仓库残留
                self.pending_commit_text = Some(state.commit_text);
                if let Some(idx) = self.active_file_tab_idx
                    && let Some(tab) = self.file_tabs.get(idx).cloned()
                {
                    self.activate_file_tab_state(tab);
                }
                true
            }
            None => {
                // 全新仓库：清空 commit 面板，避免延续上一个仓库的草稿 / amend / sign
                self.commit_amend = false;
                self.commit_sign = false;
                self.pending_commit_text = Some(gpui::SharedString::default());
                false
            }
        }
    }
}

/// 实际打开 repo + 拉 status / 分支 / stash / tag / remote。pick_directory 与 open_recent_repo 共用
pub(super) async fn open_repo_async(
    this: &gpui::WeakEntity<VcsView>,
    driver: std::sync::Arc<dyn ramag_domain::traits::GitDriver>,
    path: std::path::PathBuf,
    cx: &mut gpui::AsyncApp,
) {
    info!(?path, "vcs: opening repo");
    let open_result = driver.open_repo(&path).await;
    let repo_config = match open_result {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "vcs: open repo failed");
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.loading_label = None;
                this.error = Some(format!("打开仓库失败: {e}"));
                cx.notify();
            });
            return;
        }
    };

    let capacity_available = this
        .update(cx, |this, cx| {
            let available = this.ensure_open_repo_capacity(&repo_config.path, cx);
            if !available {
                this.loading = false;
                this.loading_label = None;
                cx.notify();
            }
            available
        })
        .unwrap_or(false);
    if !capacity_available {
        if let Err(error) = driver.close_repo(&repo_config.id).await {
            tracing::warn!(error = %error, "close repo after tab limit rejection failed");
        }
        return;
    }

    let id = repo_config.id.clone();
    let status_fut = driver.status(&id);
    let local_fut = driver.list_branches(&id, BranchKind::Local);
    let remote_fut = driver.list_branches(&id, BranchKind::Remote);
    let (status, local, remote) = futures::future::join3(status_fut, local_fut, remote_fut).await;

    let _ = this.update(cx, |this, cx| {
        this.loading = false;
        this.loading_label = None;
        let mut repo_config = repo_config;
        // driver 返回运行时配置；名称与收藏属于用户元数据，重新打开时必须保留。
        if let Some(existing) = this
            .recent_repos
            .iter()
            .find(|existing| existing.path == repo_config.path)
        {
            repo_config.name = existing.name.clone();
            repo_config.favorite = existing.favorite;
        }
        repo_config.last_opened_at = Some(chrono::Utc::now());
        // 是否首次打开（区分「新开仓库」和「tab 切换」）
        let is_new = !this.open_repos.iter().any(|r| r.path == repo_config.path);
        this.save_current_session_to_cache(cx);
        let recent_repos = std::rc::Rc::make_mut(&mut this.recent_repos);
        if let Some(existing) = recent_repos
            .iter_mut()
            .find(|existing| existing.path == repo_config.path)
        {
            *existing = repo_config.clone();
        } else {
            recent_repos.push(repo_config.clone());
        }
        this.save_repo_async(repo_config.clone(), cx);
        this.clear_session_data();

        this.repo = Some(repo_config.clone());
        if is_new {
            this.open_repos.push(repo_config.clone());
        } else if let Some(open) = this
            .open_repos
            .iter_mut()
            .find(|open| open.path == repo_config.path)
        {
            *open = repo_config.clone();
        }
        this.persist_open_repos(cx);
        // 仓库打开成功但状态 / 分支查询失败：保留已成功部分，并明确显示失败项。
        let mut load_errors = Vec::new();
        match status {
            Ok(s) => this.status = Some(s),
            Err(e) => {
                tracing::error!(error = %e, "vcs: open repo status failed");
                this.status = None;
                load_errors.push(format!("读取工作区状态失败：{e}"));
            }
        }
        match local {
            Ok(branches) => this.local_branches = branches,
            Err(e) => {
                tracing::error!(error = %e, "vcs: open repo local branches failed");
                this.local_branches.clear();
                load_errors.push(format!("读取本地分支失败：{e}"));
            }
        }
        match remote {
            Ok(branches) => this.remote_branches = branches,
            Err(e) => {
                tracing::error!(error = %e, "vcs: open repo remote branches failed");
                this.remote_branches.clear();
                load_errors.push(format!("读取远程分支失败：{e}"));
            }
        }
        if !load_errors.is_empty() {
            this.error = Some(load_errors.join("；"));
        }
        this.active_view = ActiveView::Session;

        // 已访问过的仓库：还原文件 tab 状态；新仓库：空 tabs 让用户自己选
        let session_hit = this.restore_session_from_cache(&repo_config.path);
        // 重启后 session cache 为空：从 prefs 恢复上次的提交草稿（异步读回后经
        // pending_commit_text 让 Render 写回输入框；期间用户已输入则不覆盖）
        if !session_hit {
            let storage = this.storage.clone();
            let path = repo_config.path.clone();
            cx.spawn(async move |this, cx| {
                let draft = match storage.get_preference(&commit_draft_pref_key(&path)).await {
                    Ok(Some(draft)) if !draft.is_empty() => draft,
                    Ok(_) => return,
                    Err(e) => {
                        tracing::warn!(error = %e, "load commit draft failed");
                        return;
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    let same_repo = this.repo.as_ref().is_some_and(|r| r.path == path);
                    let untouched = this.commit_input.read(cx).value().trim().is_empty();
                    if same_repo && untouched {
                        this.pending_commit_text = Some(draft.into());
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        // session 只恢复“打开了哪些 tab”，内容必须基于本次刚读取的仓库状态重拉。
        if let Some(tab) = this
            .active_file_tab_idx
            .and_then(|idx| this.file_tabs.get(idx))
            .cloned()
        {
            match tab.source {
                FileTabSource::Changes(kind) => this.select_file(tab.path, kind, cx),
                FileTabSource::ProjectFiles => this.select_pf_file(tab.path, cx),
                FileTabSource::Commit { commit_id, .. } => {
                    this.select_commit_file(tab.path, commit_id, cx);
                }
            }
        }
        // 启动该仓库的文件系统监听（旧仓库的 watcher 在内部先 drop）
        this.start_fs_watcher(cx);
        cx.notify();
        this.reload_stashes(cx);
        this.reload_tags(cx);
        this.reload_remotes(cx);
        this.reload_project_files(cx);
        // 切仓库后 clear_session_data 已清空 history_commits；若下半 pane 处于打开态，
        // 立即拉新仓库首页，避免用户看到「空 commit 列表」（原行为只有手动 toggle 才 lazy load）
        if this.history_pane_visible && this.repo.is_some() {
            this.load_history_page(0, cx);
        }
    });
}

/// 提交草稿的 prefs key（按仓库 path 隔离）
pub(super) fn commit_draft_pref_key(path: &str) -> String {
    format!("vcs_commit_draft:{path}")
}

fn strip_file_tab_payloads(file_tabs: &mut [FileTab]) {
    for tab in file_tabs {
        tab.cached_diff = None;
        tab.cached_content = None;
    }
}

fn cache_repo_session(
    cache: &mut std::collections::HashMap<String, RepoSessionState>,
    order: &mut std::collections::VecDeque<String>,
    path: String,
    state: RepoSessionState,
) {
    touch_repo_session(order, &path);
    cache.insert(path, state);
    while cache.len() > REPO_SESSION_CACHE_LIMIT {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        cache.remove(&oldest);
    }
}

fn touch_repo_session(order: &mut std::collections::VecDeque<String>, path: &str) {
    if let Some(index) = order.iter().position(|entry| entry == path) {
        order.remove(index);
    }
    order.push_back(path.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_session_cache_is_bounded_and_lru() {
        let mut cache = std::collections::HashMap::new();
        let mut order = std::collections::VecDeque::new();
        for index in 0..REPO_SESSION_CACHE_LIMIT {
            cache_repo_session(
                &mut cache,
                &mut order,
                format!("repo-{index}"),
                RepoSessionState::default(),
            );
        }
        cache_repo_session(
            &mut cache,
            &mut order,
            "repo-0".into(),
            RepoSessionState::default(),
        );
        cache_repo_session(
            &mut cache,
            &mut order,
            "repo-new".into(),
            RepoSessionState::default(),
        );

        assert_eq!(cache.len(), REPO_SESSION_CACHE_LIMIT);
        assert!(cache.contains_key("repo-0"));
        assert!(!cache.contains_key("repo-1"));
    }

    #[test]
    fn repo_session_drops_loaded_file_payloads() {
        let mut tabs = vec![FileTab {
            path: "src/lib.rs".into(),
            source: FileTabSource::ProjectFiles,
            cached_diff: None,
            cached_content: Some(super::super::helpers::FileContentSnapshot {
                path: "src/lib.rs".into(),
                lines: std::rc::Rc::new(vec!["content".into()]),
                max_chars: 7,
                truncated: false,
                binary: false,
                error: None,
            }),
        }];

        strip_file_tab_payloads(&mut tabs);

        assert!(tabs[0].cached_content.is_none());
        assert!(tabs[0].cached_diff.is_none());
    }
}

mod admin;
/// 在 worker 线程同步读盘 + 二进制 / 截断检测 → 跨线程 Send 的 [`RawFileContent`]
mod file_io;
use file_io::finalize_file_snapshot;
// untracked 伪 diff 预览（vcs_view_ops_file_tab）复用同一读盘函数
pub(in crate::views) use file_io::read_raw_file_content;
