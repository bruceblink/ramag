use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{AppContext as _, Context};
use gpui_component::resizable::ResizableState;
use ramag_domain::entities::MAX_COMMIT_MESSAGE_BYTES;
use tracing::{error, info};

use super::helpers::{
    ActiveView, FileContentSnapshot, FileTab, FileTabSource, FilesViewMode, PendingFileEditorLoad,
};
use super::vcs_view::{RepoSessionState, VcsView};

pub(super) const PF_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 编辑停止后的自动保存延迟。
const PF_FILE_AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(300);
/// 自写文件事件的忽略时长。
pub(super) const PF_FILE_SELF_WRITE_TTL: Duration = Duration::from_secs(2);
pub(super) const MAX_OPEN_REPOS: usize = 32;
const REPO_SESSION_CACHE_LIMIT: usize = 32;

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

    /// 检查提交草稿大小。
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

    /// 确认项目文件草稿已保存。
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
                    tracing::warn!(
                        operation = "vcs_status_refresh",
                        repo_id = %repo,
                        error = %e,
                        "background status refresh failed"
                    );
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
}

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

/// 仅在写入版本匹配时清除草稿标记。
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
mod tests;

mod admin;
mod file_io;
mod project_files_ops;
mod repo_open;
mod session_ops;
pub(in crate::views) use file_io::read_raw_file_content;
use file_io::{finalize_file_snapshot, prepare_file_snapshot, write_project_file};
pub(super) use repo_open::open_repo_async;
