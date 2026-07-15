//! 仓库 storage 管理 + Clone / Init / 确认弹窗

use std::collections::HashSet;

use gpui::{Context, Window};
use ramag_domain::entities::{RepoConfig, RepoId};
use ramag_domain::error::{DomainError, Result};

use super::super::vcs_view::VcsView;
use super::{MAX_OPEN_REPOS, open_repo_async};

/// 打开中的仓库 Tab 路径列表的偏好 key（JSON 数组，跨重启恢复用）
const OPEN_REPOS_PREF: &str = "vcs_open_repos";
const MAX_OPEN_REPOS_PREF_BYTES: usize = 256 * 1024;
const MAX_OPEN_REPO_PATH_BYTES: usize = 32 * 1024;

impl VcsView {
    /// 收藏 / 取消收藏最近仓库，并立即持久化。
    pub(crate) fn toggle_repo_favorite(&mut self, path: String, cx: &mut Context<Self>) {
        let repo = std::rc::Rc::make_mut(&mut self.recent_repos)
            .iter_mut()
            .find(|repo| repo.path == path)
            .map(|repo| {
                repo.favorite = !repo.favorite;
                repo.clone()
            });
        if let Some(repo) = repo {
            self.save_repo_async(repo, cx);
            cx.notify();
        }
    }

    /// 保存单条 RepoConfig 到 storage；失败要可见，否则收藏 / 最近时间会在重启后悄悄丢失。
    pub(crate) fn save_repo_async(&self, repo: RepoConfig, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = storage.save_repo(&repo).await {
                tracing::warn!(error = %e, repo = %repo.name, "vcs: save_repo failed");
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("仓库记录未能保存（重启后设置可能丢失）：{e}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 从 storage 删除单条 RepoConfig。失败要提示：内存列表已移除，
    /// 持久化没删掉会在下次启动"复活"，静默会让用户以为删除生效了
    pub(crate) fn delete_repo_async(&self, id: RepoId, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = storage.delete_repo(&id).await {
                tracing::warn!(error = %e, repo_id = %id, "vcs: delete_repo failed");
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("移除记录未能持久化（重启后可能重新出现）：{e}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 弹确认对话框：从最近列表移除仓库（不删磁盘文件）
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
            format!("确定从最近列表移除「{name}」吗？\n仅清除本地最近记录，不会删除磁盘文件。"),
            "移除",
            true,
            move |_window, app| {
                view.update(app, |this, cx| this.remove_recent_repo(path, cx));
            },
            window,
            cx,
        );
    }

    /// 异步 Clone 远程仓库到本地路径，完成后复用 open_repo_async 走 open + 拉数据流
    pub(crate) fn clone_repo_async(
        &mut self,
        url: String,
        dest: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_open_repo_capacity(&dest.to_string_lossy(), cx) {
            return;
        }
        let driver = self.driver.clone();
        self.loading = true;
        let repo_hint = url
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .unwrap_or("仓库")
            .trim_end_matches(".git");
        self.loading_label = Some(format!("正在 Clone {repo_hint} 到 {}…", dest.display()));
        self.error = None;
        self.show_clone_panel = false;
        // 进度槽 + 取消位：infra 持续写进度，loading 屏每帧读；取消钮置位后 kill 子进程
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.clone_cancel = Some(cancel.clone());
        self.clone_progress = Some(progress.clone());
        cx.notify();
        // 进度刷新 ticker：进度槽由后台线程写入，须周期 notify 驱动 loading 屏重渲染；
        // clone 结束（槽被清空）自动退出
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        let alive = this.clone_progress.is_some();
                        if alive {
                            cx.notify();
                        }
                        alive
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let was_cancelled = cancel.clone();
            let destination = dest.clone();
            let prepared = ramag_app::run_blocking(move || {
                prepare_clone_destination(&destination)
            })
            .await;
            if let Err(error) = prepared {
                tracing::error!(error = %error, dir = %dest.display(), "vcs: prepare clone destination failed");
                let _ = this.update(cx, |this, cx| {
                    this.loading = false;
                    this.loading_label = None;
                    this.clone_cancel = None;
                    this.clone_progress = None;
                    this.error = Some(format!("无法准备 Clone 目标目录：{error}"));
                    cx.notify();
                });
                return;
            }
            if was_cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = this.update(cx, |this, cx| {
                    this.loading = false;
                    this.loading_label = None;
                    this.clone_cancel = None;
                    this.clone_progress = None;
                    this.pending_clone_cleanup = Some(dest.clone());
                    cx.notify();
                });
                return;
            }
            match driver
                .clone_repo_streaming(&url, &dest, cancel, progress)
                .await
            {
                Ok(rc) => {
                    tracing::info!("vcs clone done");
                    let _ = this.update(cx, |this, cx| {
                        this.clone_cancel = None;
                        this.clone_progress = None;
                        cx.notify();
                    });
                    open_repo_async(&this, driver, std::path::PathBuf::from(&rc.path), cx).await;
                }
                Err(e) => {
                    let cancelled = was_cancelled.load(std::sync::atomic::Ordering::Relaxed);
                    if cancelled {
                        tracing::info!("vcs clone cancelled by user");
                    } else {
                        tracing::error!(error = %e, "vcs: clone failed");
                    }
                    let _ = this.update(cx, |this, cx| {
                        this.loading = false;
                        this.loading_label = None;
                        this.clone_cancel = None;
                        this.clone_progress = None;
                        // 用户主动取消：静默回列表，不当错误弹横幅；
                        // 半成品目录（本次任务独占创建）交用户决定删除或保留
                        if !cancelled {
                            this.error = Some(format!("Clone 失败: {e}"));
                        } else {
                            this.pending_clone_cleanup = Some(dest.clone());
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 用户确认后删除本次 Clone 独占创建的残留目录；文件 I/O 放共享受限工作池。
    pub(crate) fn cleanup_cancelled_clone_dir_async(
        &mut self,
        dir: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        let display = dir.display().to_string();
        cx.spawn(async move |this, cx| {
            let path = dir.clone();
            let result =
                ramag_app::run_blocking(move || remove_cancelled_clone_directory(&path)).await;
            let _ = this.update(cx, |this, cx| {
                this.pending_notification = Some(match result {
                    Ok(()) => {
                        tracing::info!(dir = %dir.display(), "cancelled clone dir removed");
                        gpui_component::notification::Notification::success(format!(
                            "已删除未完成的 Clone 目录：{display}"
                        ))
                        .autohide(true)
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, dir = %dir.display(), "cleanup cancelled clone failed");
                        gpui_component::notification::Notification::error(format!(
                            "删除未完成的 Clone 目录失败：{error}"
                        ))
                        .autohide(false)
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// 异步初始化空仓库（真正执行 git init），完成后打开 session。
    /// 目录已是 git 仓库时 init 幂等无害（git init 对既有仓库安全）
    pub(crate) fn init_repo_async(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        if !self.ensure_open_repo_capacity(&path.to_string_lossy(), cx) {
            return;
        }
        let driver = self.driver.clone();
        self.loading = true;
        self.loading_label = Some(format!("正在初始化仓库 {}…", path.display()));
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(e) = driver.init_repo(&path).await {
                tracing::error!(error = %e, path = %path.display(), "vcs: git init failed");
                let _ = this.update(cx, |this, cx| {
                    this.loading = false;
                    this.loading_label = None;
                    this.error = Some(format!("初始化仓库失败：{e}"));
                    cx.notify();
                });
                return;
            }
            open_repo_async(&this, driver, path, cx).await;
        })
        .detach();
    }

    /// 启动时从 storage 加载 recent_repos（跨重启保留），并按偏好恢复上次打开的仓库 Tab
    /// （仅恢复 Tab 列表，不自动 open 任何仓库——停留在仓库管理页，点 Tab 才真正打开）
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
                        tracing::warn!(error = %error, "parse open repos preference failed");
                        (
                            Vec::new(),
                            Some("已忽略损坏的仓库标签恢复数据".into()),
                            false,
                        )
                    }
                },
                Ok(None) => (Vec::new(), None, false),
                Err(error) => {
                    tracing::warn!(error = %error, "load open repos preference failed");
                    (
                        Vec::new(),
                        Some(format!("无法恢复上次打开的仓库标签：{error}")),
                        false,
                    )
                }
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(list) => {
                    // 按保存顺序恢复 Tab；已从 recent 移除的仓库自动跳过
                    this.open_repos = open_paths
                        .iter()
                        .filter_map(|p| list.iter().find(|r| &r.path == p).cloned())
                        .collect();
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
                    tracing::warn!(error = %e, "vcs: list_repos failed");
                    this.error = Some(format!("加载最近仓库失败：{e}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 把当前打开的仓库 Tab 路径列表落 prefs；同 key 串行且只保留最新快照。
    pub(crate) fn persist_open_repos(&self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.open_repos.iter().map(|r| r.path.clone()).collect();
        let json = match serde_json::to_string(&paths) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "vcs: serialize open repos failed");
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

fn prepare_clone_destination(path: &std::path::Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(DomainError::InvalidConfig(format!(
                "目标目录已存在：{}；为避免覆盖或误删，请选择其他父目录或调整仓库名",
                path.display()
            )))
        }
        Err(error) => Err(DomainError::Other(format!(
            "创建 Clone 目标目录 {} 失败：{error}",
            path.display()
        ))),
    }
}

fn remove_cancelled_clone_directory(path: &std::path::Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DomainError::Other(format!(
                "检查残留目录 {} 失败：{error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DomainError::InvalidConfig(format!(
            "路径已不再是本次 Clone 创建的普通目录，已拒绝删除：{}",
            path.display()
        )));
    }

    let git_path = path.join(".git");
    match std::fs::symlink_metadata(&git_path) {
        Ok(git_metadata) if git_metadata.is_dir() && !git_metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|error| {
                DomainError::Other(format!(
                    "删除残留 Clone 目录 {} 失败：{error}",
                    path.display()
                ))
            })
        }
        Ok(_) => Err(DomainError::InvalidConfig(format!(
            "残留目录中的 .git 类型异常，已拒绝删除：{}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut entries = std::fs::read_dir(path).map_err(|error| {
                DomainError::Other(format!("读取残留目录 {} 失败：{error}", path.display()))
            })?;
            match entries.next() {
                None => std::fs::remove_dir(path).map_err(|error| {
                    DomainError::Other(format!(
                        "删除空的 Clone 目标目录 {} 失败：{error}",
                        path.display()
                    ))
                }),
                Some(Ok(_)) => Err(DomainError::InvalidConfig(format!(
                    "目录内容已变化且不含 .git，已拒绝自动删除：{}",
                    path.display()
                ))),
                Some(Err(error)) => Err(DomainError::Other(format!(
                    "检查残留目录 {} 内容失败：{error}",
                    path.display()
                ))),
            }
        }
        Err(error) => Err(DomainError::Other(format!(
            "检查残留目录 {} 的 .git 失败：{error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_OPEN_REPOS, MAX_OPEN_REPOS_PREF_BYTES, parse_open_repo_paths,
        prepare_clone_destination, remove_cancelled_clone_directory,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_root(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "ramag-vcs-clone-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn clone_destination_must_not_preexist() -> std::io::Result<()> {
        let root = test_root("existing");
        let target = root.join("repo");
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join("keep.txt"), b"keep")?;

        assert!(prepare_clone_destination(&target).is_err());
        assert!(target.join("keep.txt").exists());

        std::fs::remove_dir_all(root)
    }

    #[test]
    fn cleanup_removes_empty_owned_directory() -> std::io::Result<()> {
        let root = test_root("empty");
        let target = root.join("repo");
        std::fs::create_dir_all(&target)?;

        assert!(remove_cancelled_clone_directory(&target).is_ok());
        assert!(!target.exists());

        std::fs::remove_dir(root)
    }

    #[test]
    fn cleanup_refuses_changed_non_git_directory() -> std::io::Result<()> {
        let root = test_root("changed");
        let target = root.join("repo");
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join("user-file.txt"), b"keep")?;

        assert!(remove_cancelled_clone_directory(&target).is_err());
        assert!(target.join("user-file.txt").exists());

        std::fs::remove_dir_all(root)
    }

    #[test]
    fn cleanup_removes_partial_git_clone() -> std::io::Result<()> {
        let root = test_root("partial");
        let target = root.join("repo");
        std::fs::create_dir_all(target.join(".git"))?;
        std::fs::write(target.join("partial.txt"), b"partial")?;

        assert!(remove_cancelled_clone_directory(&target).is_ok());
        assert!(!target.exists());

        std::fs::remove_dir(root)
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_refuses_symbolic_link() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        let outside = root.with_extension("outside");
        std::fs::create_dir_all(&outside)?;
        std::fs::create_dir_all(&root)?;
        let target = root.join("repo");
        symlink(&outside, &target)?;

        assert!(remove_cancelled_clone_directory(&target).is_err());
        assert!(outside.exists());

        std::fs::remove_file(target)?;
        std::fs::remove_dir(root)?;
        std::fs::remove_dir(outside)
    }

    #[test]
    fn open_repo_paths_are_bounded_and_deduplicated() {
        let mut paths = vec!["/repo/0".to_string(), "/repo/0".to_string()];
        paths.extend((1..=MAX_OPEN_REPOS).map(|index| format!("/repo/{index}")));
        let json = serde_json::to_string(&paths).unwrap_or_default();

        assert!(matches!(
            parse_open_repo_paths(&json),
            Ok((paths, true))
                if paths.len() == MAX_OPEN_REPOS
                    && paths.first().is_some_and(|path| path == "/repo/0")
        ));
        assert!(parse_open_repo_paths(&" ".repeat(MAX_OPEN_REPOS_PREF_BYTES + 1)).is_err());
    }
}
