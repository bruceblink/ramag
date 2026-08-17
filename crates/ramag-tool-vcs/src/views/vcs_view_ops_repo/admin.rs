//! 仓库记录、克隆与初始化。

use std::collections::HashSet;

use gpui::{Context, Window};
use ramag_domain::entities::{RepoConfig, RepoId};
use ramag_domain::error::{DomainError, Result};

use super::super::vcs_view::VcsView;
use super::MAX_OPEN_REPOS;

const OPEN_REPOS_PREF: &str = "vcs_open_repos";
const MAX_OPEN_REPOS_PREF_BYTES: usize = 256 * 1024;
const MAX_OPEN_REPO_PATH_BYTES: usize = 32 * 1024;

mod creation;

impl VcsView {
    /// 异步保存仓库记录。
    pub(crate) fn save_repo_async(&self, repo: RepoConfig, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        let coordinator = self.repo_write_coordinator.clone();
        let ticket = coordinator.begin(repo.path.clone());
        cx.spawn(async move |this, cx| {
            let result = coordinator
                .run_if_latest(&ticket, || storage.save_repo(&repo))
                .await;
            if let Some(Err(e)) = result {
                tracing::warn!(
                    operation = "git_repo_save",
                    repo_id = %repo.id,
                    repo = %repo.name,
                    error = %e,
                    "save repository failed"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("仓库记录未能保存（重启后设置可能丢失）：{e}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 异步删除仓库记录。
    pub(crate) fn delete_repo_async(&self, path: String, id: RepoId, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        let coordinator = self.repo_write_coordinator.clone();
        let ticket = coordinator.begin(path);
        cx.spawn(async move |this, cx| {
            let result = coordinator
                .run_if_latest(&ticket, || storage.delete_repo(&id))
                .await;
            if let Some(Err(e)) = result {
                tracing::warn!(
                    operation = "git_repo_delete",
                    repo_id = %id,
                    error = %e,
                    "delete repository failed"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("移除记录未能持久化（重启后可能重新出现）：{e}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 确认后从最近列表移除仓库，不删除磁盘文件。
    pub(crate) fn confirm_remove_recent_repo(
        &self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        let name = self
            .recent_repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| path.clone());
        ramag_ui::open_confirm(
            "从最近列表移除？",
            format!("移除「{name}」的本机最近记录；不会删除磁盘文件。"),
            "移除",
            true,
            move |_window, app| {
                view.update(app, |this, cx| this.remove_recent_repo(path, cx));
            },
            window,
            cx,
        );
    }

    /// 加载最近仓库及上次打开的标签，不自动打开仓库。
    pub(crate) fn load_recent_repos_async(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let storage = match this.update(cx, |this, _| this.storage.clone()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let result = storage.list_repos().await;
            let (open_paths, open_paths_error, open_paths_adjusted): (
                Vec<String>,
                Option<String>,
                bool,
            ) = match storage.get_preference(OPEN_REPOS_PREF).await {
                Ok(Some(json)) => match parse_open_repo_paths(&json) {
                    Ok((paths, adjusted)) => (paths, None, adjusted),
                    Err(error) => {
                        tracing::warn!(
                            operation = "vcs_repository_session_load",
                            stage = "parse",
                            error = %error,
                            "parse open repos preference failed"
                        );
                        (
                            Vec::new(),
                            Some("已忽略损坏的仓库标签恢复数据".into()),
                            false,
                        )
                    }
                },
                Ok(None) => (Vec::new(), None, false),
                Err(error) => {
                    tracing::warn!(
                        operation = "vcs_repository_session_load",
                        stage = "read_preference",
                        error = %error,
                        "load open repos preference failed"
                    );
                    (
                        Vec::new(),
                        Some(format!("无法恢复上次打开的仓库标签：{error}")),
                        false,
                    )
                }
            };
            let _ = this.update(cx, |this, cx| {
                let restore_allowed = this.startup_repo_restore_allowed;
                this.startup_repo_restore_allowed = false;
                match result {
                    Ok(mut list) => {
                        if restore_allowed {
                            this.open_repos = open_paths
                                .iter()
                                .filter_map(|p| list.iter().find(|r| &r.path == p).cloned())
                                .collect();
                        } else {
                            for current in this.recent_repos.iter() {
                                if let Some(loaded) =
                                    list.iter_mut().find(|loaded| loaded.path == current.path)
                                {
                                    *loaded = current.clone();
                                } else {
                                    list.push(current.clone());
                                }
                            }
                        }
                        this.recent_repos = std::rc::Rc::new(list);
                        this.repo_list_rows_cache.get_mut().take();
                        if let Some(error) = open_paths_error {
                            this.error = Some(error);
                        }
                        if open_paths_adjusted {
                            this.pending_notification = Some(
                            gpui_component::notification::Notification::warning(format!(
                                "上次仓库标签包含重复或超限项，仅恢复前 {MAX_OPEN_REPOS} 个有效标签"
                            ))
                            .autohide(true),
                        );
                        }
                        cx.notify();
                    }
                    Err(e) => {
                        tracing::warn!(
                            operation = "vcs_repository_session_load",
                            error = %e,
                            "load repositories failed"
                        );
                        if restore_allowed {
                            this.error = Some(format!("加载最近仓库失败：{e}"));
                            cx.notify();
                        }
                    }
                }
            });
        })
        .detach();
    }

    /// 保存已打开仓库的标签列表。
    pub(crate) fn persist_open_repos(&self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.open_repos.iter().map(|r| r.path.clone()).collect();
        let json = match serde_json::to_string(&paths) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(
                    operation = "vcs_repository_session_save",
                    error = %e,
                    "serialize repository session failed"
                );
                return;
            }
        };
        ramag_ui::preferences::persist_preference_latest_with_storage(
            OPEN_REPOS_PREF,
            json,
            self.storage.clone(),
            cx,
        );
    }
}

fn parse_open_repo_paths(json: &str) -> Result<(Vec<String>, bool)> {
    if json.len() > MAX_OPEN_REPOS_PREF_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "仓库标签恢复数据过大：{} bytes",
            json.len()
        )));
    }
    let paths = serde_json::from_str::<Vec<String>>(json)
        .map_err(|error| DomainError::InvalidConfig(format!("仓库标签恢复数据无效：{error}")))?;
    let original_len = paths.len();
    let mut seen = HashSet::with_capacity(original_len.min(MAX_OPEN_REPOS));
    let mut normalized = Vec::with_capacity(original_len.min(MAX_OPEN_REPOS));
    let mut adjusted = false;
    for path in paths {
        if path.is_empty() {
            adjusted = true;
            continue;
        }
        if path.len() > MAX_OPEN_REPO_PATH_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "仓库标签路径过长：{} bytes",
                path.len()
            )));
        }
        if normalized.len() >= MAX_OPEN_REPOS {
            adjusted = true;
            break;
        }
        if seen.insert(path.clone()) {
            normalized.push(path);
        } else {
            adjusted = true;
        }
    }
    adjusted |= normalized.len() != original_len;
    Ok((normalized, adjusted))
}

#[cfg(test)]
mod tests;
