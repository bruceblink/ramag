//! 仓库 / Session 管理 ops：pick_directory / open_recent_repo / remove_recent_repo /
//! remove_open_repo / open_repo_async（共享异步流：open + 拉 status / 分支 / stash 等）

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::Context;
use ramag_domain::entities::MAX_COMMIT_MESSAGE_BYTES;
use tracing::{error, info};

use super::helpers::{
    ActiveView, FileContentSnapshot, FileTab, FileTabSource, FilesViewMode, PendingFileEditorLoad,
};
use super::vcs_view::{RepoSessionState, VcsView};

/// Project Files 点击文件后读盘上限（4MB）；超过截断后 UI 显示提示
pub(super) const PF_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 连续输入只在停顿后写盘，避免每个字符都触发 truncate + sync_all。
const PF_FILE_AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(300);
/// 自身写盘事件的短期识别窗口；过期后必须按外部修改处理。
pub(super) const PF_FILE_SELF_WRITE_TTL: Duration = Duration::from_secs(2);
/// 单窗口打开仓库标签上限；每个标签会保留会话元数据，并可能在驱动层持有仓库句柄。
pub(super) const MAX_OPEN_REPOS: usize = 32;
/// 仅保留最近访问仓库的轻量 UI 会话，避免长时间运行后路径与草稿无限累积。
const REPO_SESSION_CACHE_LIMIT: usize = 32;

/// worker 线程跨线程返回结构（Send）；主线程 finalize 后包 Rc 成 FileContentSnapshot
pub(super) struct RawFileContent {
    pub(super) path: String,
    pub(super) lines: Vec<String>,
    pub(super) truncated: bool,
    pub(super) binary: bool,
    pub(super) error: Option<String>,
}

impl RawFileContent {
    pub(in crate::views) fn with_error(path: String, error: String) -> Self {
        Self {
            path,
            lines: Vec::new(),
            truncated: false,
            binary: false,
            error: Some(error),
        }
    }
}

impl VcsView {
    /// 弹出系统目录选择器；用户选完后异步打开仓库
    pub(super) fn pick_directory(&mut self, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        if self.loading || self.directory_picker_busy {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，完成后再切换仓库", cx);
            return;
        }
        if !self.ensure_commit_draft_within_limit(cx) || !self.ensure_project_file_drafts_saved(cx)
        {
            return;
        }
        let driver = self.driver.clone();
        self.directory_picker_busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_title("选择 Git 仓库目录")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf());
            let Some(path) = path else {
                let _ = this.update(cx, |this, cx| {
                    this.directory_picker_busy = false;
                    cx.notify();
                });
                return;
            };
            let should_open = this
                .update(cx, |this, cx| {
                    this.directory_picker_busy = false;
                    if this.loading {
                        cx.notify();
                        return false;
                    }
                    if this.busy {
                        this.notify_warning("当前 Git 写操作尚未完成，完成后再切换仓库", cx);
                        return false;
                    }
                    if !this.ensure_commit_draft_within_limit(cx)
                        || !this.ensure_project_file_drafts_saved(cx)
                        || !this.ensure_open_repo_capacity(&path.to_string_lossy(), cx)
                    {
                        return false;
                    }
                    this.loading = true;
                    this.error = None;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !should_open {
                return;
            }
            open_repo_async(&this, driver, path, cx).await;
        })
        .detach();
    }

    /// 异步选择待初始化目录，避免原生对话框阻塞 GPUI 前台线程。
    pub(super) fn pick_init_directory(&mut self, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        if self.loading || self.directory_picker_busy {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，完成后再初始化仓库", cx);
            return;
        }
        if !self.ensure_commit_draft_within_limit(cx) || !self.ensure_project_file_drafts_saved(cx)
        {
            return;
        }
        self.directory_picker_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_title("选择或新建仓库目录")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf());
            let _ = this.update(cx, |this, cx| {
                this.directory_picker_busy = false;
                if let Some(path) = path {
                    this.init_repo_async(path, cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 异步选择 Clone 父目录；只更新表单，不启动网络操作。
    pub(super) fn pick_clone_destination(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.busy || self.directory_picker_busy {
            return;
        }
        self.directory_picker_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = rfd::AsyncFileDialog::new()
                .set_title("选择 Clone 目标目录")
                .pick_folder()
                .await
                .map(|handle| handle.path().to_path_buf());
            let _ = this.update(cx, |this, cx| {
                this.directory_picker_busy = false;
                if let Some(path) = path {
                    this.clone_dest_path = Some(path);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从最近列表点击仓库行 → 直接打开（不弹文件对话框）
    pub(super) fn open_recent_repo(&mut self, path: String, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        if self.loading {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，完成后再切换仓库", cx);
            return;
        }
        let switching_repo = self.repo.as_ref().is_some_and(|repo| repo.path != path);
        if !self.ensure_commit_draft_within_limit(cx)
            || (switching_repo && !self.ensure_project_file_drafts_saved(cx))
        {
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

    /// 超限草稿既不能提交，也不能安全进入有界会话缓存；先让用户缩短，避免切仓时丢稿。
    pub(super) fn ensure_commit_draft_within_limit(&mut self, cx: &mut Context<Self>) -> bool {
        if self.repo.is_none()
            || self.commit_input.read(cx).value().len() <= MAX_COMMIT_MESSAGE_BYTES
        {
            return true;
        }
        let message = format!(
            "提交草稿超过 {} MiB 上限，尚未保存；请缩短后再切换或关闭仓库",
            MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
        );
        self.commit_draft_error = Some(message.clone());
        self.notify_warning(message, cx);
        false
    }

    /// 自动保存尚未完成时不允许切仓或关闭，避免异步写入失去所属会话。
    pub(super) fn ensure_project_file_drafts_saved(&mut self, cx: &mut Context<Self>) -> bool {
        self.capture_active_project_draft(cx);
        let dirty_paths = self
            .file_tabs
            .iter()
            .filter(|tab| tab.is_dirty())
            .map(|tab| tab.path.clone())
            .collect::<Vec<_>>();
        if dirty_paths.is_empty() {
            return true;
        }

        let preview = dirty_paths
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("、");
        let suffix = if dirty_paths.len() > 3 { " 等" } else { "" };
        self.notify_warning(
            format!(
                "有 {} 个文件尚未完成自动保存（{preview}{suffix}），请稍后再切换或关闭仓库",
                dirty_paths.len()
            ),
            cx,
        );
        false
    }

    /// 从最近列表移除（不删磁盘）；按 path 找 RepoId 后调 storage.delete_repo
    pub(super) fn remove_recent_repo(&mut self, path: String, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        let repo_id = self
            .recent_repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.id.clone());
        std::rc::Rc::make_mut(&mut self.recent_repos).retain(|r| r.path != path);
        if let Some(id) = repo_id {
            self.delete_repo_async(path, id, cx);
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
                    // driver 已在受限 Git worker 中按字母序整理，UI 只交换结果。
                    Ok(paths) => this.project_files = paths,
                    Err(e) => {
                        error!(error = %e, "load project files failed");
                        // 失败时仍清空避免显示旧数据；错误以 banner 形式提示
                        this.project_files = Vec::new();
                        this.error = Some(format!("加载 Project Files 失败: {e}"));
                    }
                }
                this.prune_project_expanded_dirs();
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
        let same_target = existing.is_some_and(|idx| {
            self.active_file_tab_idx == Some(idx)
                && self.selected_pf_path.as_deref() == Some(path.as_str())
        });
        if same_target
            && (existing
                .and_then(|idx| self.file_tabs.get(idx))
                .is_some_and(|tab| tab.cached_content.is_some())
                || self.loading_file_content)
        {
            return;
        }
        self.capture_active_project_draft(cx);
        self.diff_fullscreen = false;
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
        let is_new_tab = existing.is_none();
        let idx = if let Some(i) = existing {
            i
        } else {
            self.file_tabs.push(FileTab {
                path: path.clone(),
                source: FileTabSource::ProjectFiles,
                cached_diff: None,
                cached_diff_syntax: None,
                cached_content: None,
            });
            self.file_tabs.len() - 1
        };
        if is_new_tab {
            self.scroll_file_tabs_to_end();
        }
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
            let prepared = match ramag_app::run_blocking(move || {
                let raw = read_raw_file_content(&repo_root, &path_for_worker);
                Ok(prepare_file_snapshot(raw))
            })
            .await
            {
                Ok(prepared) => prepared,
                Err(e) => prepare_file_snapshot(RawFileContent::with_error(
                    path.clone(),
                    format!("文件读取任务失败: {e}"),
                )),
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id)
                    || this.file_content_request_seq != request_seq
                    || this.selected_pf_path.as_deref() != Some(path.as_str())
                {
                    return;
                }
                let snapshot = Some(finalize_file_snapshot(prepared));
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
                    if let Some(snapshot) = snapshot.as_ref() {
                        this.queue_project_editor_load(snapshot);
                    }
                    this.current_file_content = snapshot;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 切换标签前把当前编辑器草稿写回对应 tab 的内存快照，不触碰磁盘。
    pub(super) fn capture_active_project_draft(&mut self, cx: &mut Context<Self>) {
        if !self.pf_editor_dirty {
            return;
        }
        let Some(path) = self.pf_editor_loaded_path.clone() else {
            return;
        };
        if self.selected_pf_path.as_deref() != Some(path.as_str()) {
            return;
        }
        let editor = self.pf_editor.read(cx);
        let text = std::rc::Rc::new(editor.value().to_string());
        let line_count = editor.text().len_lines(ropey::LineType::LF);
        let Some(tab) = self
            .file_tabs
            .iter_mut()
            .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)
        else {
            return;
        };
        let Some(mut snapshot) = tab.cached_content.clone() else {
            return;
        };
        snapshot.text = text;
        snapshot.line_count = line_count;
        snapshot.revision = self.pf_editor_revision;
        snapshot.dirty = true;
        tab.cached_content = Some(snapshot.clone());
        self.pf_editor_line_count = line_count;
        self.current_file_content = Some(snapshot);
    }

    /// 输入事件只更新轻量元数据，让标签蓝点立即出现；正文复制留到防抖命中时。
    pub(super) fn mark_active_project_file_dirty(&mut self) {
        let Some(path) = self.pf_editor_loaded_path.as_deref() else {
            return;
        };
        if self.selected_pf_path.as_deref() != Some(path) {
            return;
        }
        let Some(tab) = self
            .file_tabs
            .iter_mut()
            .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)
        else {
            return;
        };
        let Some(snapshot) = tab.cached_content.as_mut() else {
            return;
        };
        snapshot.line_count = self.pf_editor_line_count;
        snapshot.revision = self.pf_editor_revision;
        snapshot.dirty = true;
        self.current_file_content = Some(snapshot.clone());
    }

    /// 为 Render 安排一次编辑器正文切换；set_value 会同时把编辑器滚动位置复位到顶部。
    pub(super) fn queue_project_editor_load(&mut self, snapshot: &FileContentSnapshot) {
        self.pf_editor_loaded_path = None;
        self.pf_editor_dirty = snapshot.dirty;
        self.pf_editor_revision = snapshot.revision;
        self.pf_editor_line_count = snapshot.line_count;
        self.pending_pf_editor_load = Some(PendingFileEditorLoad {
            path: snapshot.path.clone(),
            text: snapshot.text.clone(),
            language: super::syntax::lang_for_path(&snapshot.path)
                .unwrap_or("text")
                .into(),
        });
    }

    /// 输入停顿后自动保存当前 Project Files 正文。
    pub(super) fn schedule_project_file_autosave(&mut self, cx: &mut Context<Self>) {
        self.schedule_current_project_file_save(PF_FILE_AUTOSAVE_DEBOUNCE, cx);
    }

    /// 快捷键可跳过去抖并立即重试；常规编辑无需显式保存。
    pub(super) fn save_project_file(&mut self, cx: &mut Context<Self>) {
        self.schedule_current_project_file_save(Duration::ZERO, cx);
    }

    fn schedule_current_project_file_save(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if !self.pf_editor_dirty {
            return;
        }
        let Some(path) = self.selected_pf_path.clone() else {
            return;
        };
        let Some(snapshot) = self.current_file_content.as_ref() else {
            return;
        };
        if snapshot.error.is_some() || snapshot.binary || snapshot.truncated || !snapshot.dirty {
            return;
        }
        let Some((repo_path, repo_id)) = self
            .repo
            .as_ref()
            .map(|repo| (repo.path.clone(), repo.id.clone()))
        else {
            return;
        };

        let revision = self.pf_editor_revision;
        let coordinator = self.project_file_write_coordinator.clone();
        let ticket = coordinator.begin(format!("{repo_path}\0{path}"));

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            // 只在最新代际命中后复制正文；快速输入不会反复克隆最多 4 MiB 的文本。
            let text = this
                .update(cx, |this, cx| {
                    if !this.is_current_repo(&repo_id) {
                        return None;
                    }
                    if this.selected_pf_path.as_deref() == Some(path.as_str())
                        && this.pf_editor_loaded_path.as_deref() == Some(path.as_str())
                    {
                        if !this.pf_editor_dirty || this.pf_editor_revision != revision {
                            return None;
                        }
                        this.capture_active_project_draft(cx);
                    }
                    let snapshot = this
                        .file_tabs
                        .iter()
                        .find(|tab| {
                            tab.path == path && tab.source == FileTabSource::ProjectFiles
                        })?
                        .cached_content
                        .as_ref()?;
                    let text = (snapshot.dirty
                        && snapshot.revision == revision
                        && snapshot.error.is_none()
                        && !snapshot.binary
                        && !snapshot.truncated)
                        .then(|| snapshot.text.as_ref().clone())?;
                    // 写入前登记，确保 watcher 事件无论先于还是后于写盘回包都只消费一次。
                    let now = std::time::Instant::now();
                    this.project_file_self_writes
                        .retain(|_, (_, saved_at)| {
                            now.saturating_duration_since(*saved_at) <= PF_FILE_SELF_WRITE_TTL
                        });
                    this.project_file_self_writes
                        .insert(path.clone(), (revision, now));
                    Some(text)
                })
                .ok()
                .flatten();
            let Some(text) = text else {
                coordinator.cancel_if_latest(&ticket);
                return;
            };

            let root = std::path::PathBuf::from(repo_path);
            let path_for_worker = path.clone();
            let result = coordinator
                .run_if_latest(&ticket, || {
                    ramag_app::run_blocking(move || {
                        write_project_file(&root, &path_for_worker, text.as_str())
                            .map_err(ramag_domain::error::DomainError::Other)
                    })
                })
                .await;
            let Some(result) = result else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.is_current_repo(&repo_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        // 写盘期间的新编辑会提升 revision，旧回包不得清除新一代蓝点。
                        let current = mark_project_file_revision_saved(
                            &mut this.file_tabs,
                            &path,
                            revision,
                        );
                        if this.selected_pf_path.as_deref() == Some(path.as_str())
                            && let Some(snapshot) = current
                        {
                            this.pf_editor_dirty = snapshot.dirty;
                            this.current_file_content = Some(snapshot);
                        }
                        // 文件监听会按路径增量刷新 Git 状态，无需再发起一次全量刷新。
                    }
                    Err(error) => {
                        tracing::error!(error = %error, path = %path, "autosave project file failed");
                        this.pending_notification = Some(
                            gpui_component::notification::Notification::error(format!(
                                "自动保存 {path} 失败：{error}；可按 {} 重试",
                                ramag_ui::platform::primary_shortcut("S")
                            ))
                            .autohide(true),
                        );
                    }
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
                    tracing::warn!(error = %e, "background status refresh failed");
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
        self.startup_repo_restore_allowed = false;
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
            if !self.ensure_commit_draft_within_limit(cx)
                || !self.ensure_project_file_drafts_saved(cx)
            {
                return;
            }
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
                        error!(error = %e, repo_id = %repo_id, "close repository failed");
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
    pub(super) fn save_current_session_to_cache(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|r| r.path.clone()) else {
            return;
        };
        let commit_text = self.commit_input.read(cx).value();
        debug_assert!(commit_text.len() <= MAX_COMMIT_MESSAGE_BYTES);
        // 切仓即持久化当前草稿（作废在途防抖任务——其读到的将是新仓文本），重启后可恢复
        let generation = self
            .commit_draft_gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_ref = self.commit_draft_gen.clone();
        {
            let storage = self.storage.clone();
            let write_lock = self.commit_draft_write_lock.clone();
            let key = commit_draft_pref_key(&path);
            let text = commit_text.clone();
            cx.spawn(async move |this, cx| {
                let _guard = write_lock.lock().await;
                // 切仓快照必须落盘；后续新仓输入会推进全局代际，但写的是不同 key，不能取消本次。
                let result = storage.set_preference(&key, &text).await;
                if let Err(error) = &result {
                    tracing::warn!(error = %error, "persist commit draft on switch failed");
                }
                let _ = this.update(cx, |this, cx| {
                    if generation_ref.load(Ordering::Relaxed) != generation {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if this.commit_draft_error.take().is_some() {
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            this.commit_draft_error = Some(format!("提交草稿保存失败：{error}"));
                            cx.notify();
                        }
                    }
                });
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
        let Some(path) = self.repo.as_ref().map(|repo| repo.path.clone()) else {
            return;
        };
        let text = self.commit_input.read(cx).value();
        let generation = self
            .commit_draft_gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let generation_ref = self.commit_draft_gen.clone();
        if text.len() > MAX_COMMIT_MESSAGE_BYTES {
            self.commit_draft_error = Some(format!(
                "提交草稿超过 {} MiB 上限，未保存；请缩短后重试",
                MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
            ));
            cx.notify();
            return;
        }
        let storage = self.storage.clone();
        let write_lock = self.commit_draft_write_lock.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(800))
                .await;
            if generation_ref.load(Ordering::Relaxed) != generation {
                return;
            }
            let _guard = write_lock.lock().await;
            if generation_ref.load(Ordering::Relaxed) != generation {
                return;
            }
            let result = storage
                .set_preference(&commit_draft_pref_key(&path), &text)
                .await;
            if let Err(error) = &result {
                tracing::warn!(error = %error, "persist commit draft failed");
            }
            let _ = this.update(cx, |this, cx| {
                if generation_ref.load(Ordering::Relaxed) != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        if this.commit_draft_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    Err(error) => {
                        this.commit_draft_error = Some(format!("提交草稿保存失败：{error}"));
                        cx.notify();
                    }
                }
            });
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
                    tab.cached_diff_syntax = None;
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
    info!(?path, "opening repository");
    let open_result = driver.open_repo(&path).await;
    let repo_config = match open_result {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "open repository failed");
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

    // 文件对话框或远程操作期间仍做一次防御性复核，避免异步间隙中的超限草稿被切仓丢弃。
    let draft_safe = this
        .update(cx, |this, cx| {
            let switching_repo = this
                .repo
                .as_ref()
                .is_some_and(|repo| repo.path != repo_config.path);
            let safe = this.ensure_commit_draft_within_limit(cx)
                && (!switching_repo || this.ensure_project_file_drafts_saved(cx));
            if !safe {
                this.loading = false;
                this.loading_label = None;
                cx.notify();
            }
            safe
        })
        .unwrap_or(false);
    if !draft_safe {
        if let Err(error) = driver.close_repo(&repo_config.id).await {
            tracing::warn!(error = %error, "close repo after commit draft rejection failed");
        }
        return;
    }

    let id = repo_config.id.clone();
    let status_fut = driver.status(&id);
    let branches_fut = driver.list_all_branches(&id);
    let (status, branches) = futures::future::join(status_fut, branches_fut).await;

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
                tracing::error!(error = %e, "load repository status failed");
                this.status = None;
                load_errors.push(format!("读取工作区状态失败：{e}"));
            }
        }
        match branches {
            Ok((local, remote)) => {
                this.local_branches = local;
                this.remote_branches = remote;
            }
            Err(e) => {
                tracing::error!(error = %e, "load repository branches failed");
                this.local_branches.clear();
                this.remote_branches.clear();
                load_errors.push(format!("读取分支失败：{e}"));
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
                    Ok(Some(draft)) if draft.len() > MAX_COMMIT_MESSAGE_BYTES => {
                        tracing::warn!(bytes = draft.len(), "ignore oversized commit draft");
                        let _ = this.update(cx, |this, cx| {
                            this.commit_draft_error = Some(format!(
                                "已忽略超过 {} MiB 上限的历史提交草稿",
                                MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
                            ));
                            cx.notify();
                        });
                        return;
                    }
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
        tab.cached_diff_syntax = None;
        tab.cached_content = None;
    }
}

/// 仅完全匹配写盘代际时清除 dirty，避免较慢旧写覆盖后续编辑状态。
fn mark_project_file_revision_saved(
    file_tabs: &mut [FileTab],
    path: &str,
    revision: u64,
) -> Option<FileContentSnapshot> {
    let snapshot = file_tabs
        .iter_mut()
        .find(|tab| tab.path == path && tab.source == FileTabSource::ProjectFiles)?
        .cached_content
        .as_mut()?;
    if snapshot.revision == revision {
        snapshot.dirty = false;
    }
    Some(snapshot.clone())
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
            cached_diff_syntax: None,
            cached_content: Some(super::super::helpers::FileContentSnapshot {
                path: "src/lib.rs".into(),
                text: std::rc::Rc::new("content".into()),
                line_count: 1,
                revision: 0,
                dirty: false,
                truncated: false,
                binary: false,
                error: None,
            }),
        }];

        strip_file_tab_payloads(&mut tabs);

        assert!(tabs[0].cached_content.is_none());
        assert!(tabs[0].cached_diff.is_none());
    }

    #[test]
    fn completed_save_clears_only_the_matching_revision() {
        let mut tabs = vec![FileTab {
            path: "src/lib.rs".into(),
            source: FileTabSource::ProjectFiles,
            cached_diff: None,
            cached_diff_syntax: None,
            cached_content: Some(FileContentSnapshot {
                path: "src/lib.rs".into(),
                text: std::rc::Rc::new("new".into()),
                line_count: 1,
                revision: 2,
                dirty: true,
                truncated: false,
                binary: false,
                error: None,
            }),
        }];

        let stale = mark_project_file_revision_saved(&mut tabs, "src/lib.rs", 1);
        assert!(stale.is_some_and(|snapshot| snapshot.dirty));
        let current = mark_project_file_revision_saved(&mut tabs, "src/lib.rs", 2);
        assert!(current.is_some_and(|snapshot| !snapshot.dirty));
    }
}

mod admin;
/// worker 线程完成读盘、二进制 / 截断检测与语法快照准备。
mod file_io;
use file_io::{finalize_file_snapshot, prepare_file_snapshot, write_project_file};
// untracked 伪 diff 预览（vcs_view_ops_file_tab）复用同一读盘函数
pub(in crate::views) use file_io::read_raw_file_content;
