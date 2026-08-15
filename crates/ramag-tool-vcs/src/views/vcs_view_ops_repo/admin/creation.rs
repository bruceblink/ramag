//! 仓库克隆、初始化与残留目录清理。

use gpui::Context;
use ramag_domain::error::{DomainError, Result};

use super::super::super::helpers::is_current_arc_slot;
use super::super::super::vcs_view::VcsView;
use super::super::open_repo_async;

impl VcsView {
    /// 异步克隆仓库，完成后打开。
    pub(crate) fn clone_repo_async(
        &mut self,
        url: String,
        dest: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.startup_repo_restore_allowed = false;
        if self.loading {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，请稍后再克隆仓库", cx);
            return;
        }
        if !self.ensure_commit_draft_within_limit(cx) || !self.ensure_project_file_drafts_saved(cx)
        {
            return;
        }
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
        self.loading_label = Some(format!("正在克隆 {repo_hint} 到 {}…", dest.display()));
        self.error = None;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.clone_cancel = Some(cancel.clone());
        self.clone_progress = Some(progress.clone());
        cx.notify();

        let poll_cancel = cancel.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        let alive = is_current_arc_slot(this.clone_cancel.as_ref(), &poll_cancel);
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
            let destination = dest.clone();
            let prepared =
                ramag_app::run_blocking(move || prepare_clone_destination(&destination)).await;
            if let Err(error) = prepared {
                tracing::error!(
                    operation = "git_clone",
                    destination = %dest.display(),
                    error = %error,
                    "prepare clone destination failed"
                );
                let _ = this.update(cx, |this, cx| {
                    if !is_current_arc_slot(this.clone_cancel.as_ref(), &cancel) {
                        return;
                    }
                    this.loading = false;
                    this.loading_label = None;
                    this.clone_cancel = None;
                    this.clone_progress = None;
                    this.error = Some(format!("无法准备克隆目标目录：{error}"));
                    cx.notify();
                });
                return;
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = this.update(cx, |this, cx| {
                    if !is_current_arc_slot(this.clone_cancel.as_ref(), &cancel) {
                        return;
                    }
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
                .clone_repo_streaming(&url, &dest, cancel.clone(), progress)
                .await
            {
                Ok(repo) => {
                    tracing::info!(
                        operation = "git_clone",
                        repo_id = %repo.id,
                        destination = %repo.path,
                        "clone completed"
                    );
                    let current = this
                        .update(cx, |this, cx| {
                            if !is_current_arc_slot(this.clone_cancel.as_ref(), &cancel) {
                                return false;
                            }
                            this.clone_cancel = None;
                            this.clone_progress = None;
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                    open_repo_async(&this, driver, std::path::PathBuf::from(&repo.path), cx).await;
                }
                Err(error) => {
                    let cancelled = cancel.load(std::sync::atomic::Ordering::Relaxed);
                    if cancelled {
                        tracing::info!(
                            operation = "git_clone",
                            destination = %dest.display(),
                            "clone cancelled by user"
                        );
                    } else {
                        tracing::error!(
                            operation = "git_clone",
                            destination = %dest.display(),
                            error = %error,
                            "clone failed"
                        );
                    }
                    let _ = this.update(cx, |this, cx| {
                        if !is_current_arc_slot(this.clone_cancel.as_ref(), &cancel) {
                            return;
                        }
                        this.loading = false;
                        this.loading_label = None;
                        this.clone_cancel = None;
                        this.clone_progress = None;
                        // 取消后保留目录，由用户决定是否删除。
                        if cancelled {
                            this.pending_clone_cleanup = Some(dest.clone());
                        } else {
                            this.error = Some(format!("克隆失败：{error}"));
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 删除本次克隆创建的残留目录。
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
                        tracing::info!(
                            operation = "git_clone_cleanup",
                            destination = %dir.display(),
                            "partial clone directory removed"
                        );
                        gpui_component::notification::Notification::success(format!(
                            "已删除未完成的克隆目录：{display}"
                        ))
                        .autohide(true)
                    }
                    Err(error) => {
                        tracing::warn!(
                            operation = "git_clone_cleanup",
                            destination = %dir.display(),
                            error = %error,
                            "partial clone cleanup failed"
                        );
                        gpui_component::notification::Notification::error(format!(
                            "删除未完成的克隆目录失败：{error}"
                        ))
                        .autohide(false)
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// 异步初始化仓库，完成后打开。
    pub(crate) fn init_repo_async(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.startup_repo_restore_allowed = false;
        if self.loading {
            return;
        }
        if self.busy {
            self.notify_warning("当前 Git 写操作尚未完成，请稍后再初始化仓库", cx);
            return;
        }
        if !self.ensure_commit_draft_within_limit(cx) || !self.ensure_project_file_drafts_saved(cx)
        {
            return;
        }
        if !self.ensure_open_repo_capacity(&path.to_string_lossy(), cx) {
            return;
        }
        let driver = self.driver.clone();
        self.loading = true;
        self.loading_label = Some(format!("正在初始化仓库 {}…", path.display()));
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = driver.init_repo(&path).await {
                tracing::error!(
                    operation = "git_repository_init",
                    path = %path.display(),
                    error = %error,
                    "repository initialization failed"
                );
                let _ = this.update(cx, |this, cx| {
                    this.loading = false;
                    this.loading_label = None;
                    this.error = Some(format!("初始化仓库失败：{error}"));
                    cx.notify();
                });
                return;
            }
            open_repo_async(&this, driver, path, cx).await;
        })
        .detach();
    }
}

pub(super) fn prepare_clone_destination(path: &std::path::Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(DomainError::InvalidConfig(format!(
                "目标目录已存在：{}；为避免覆盖，请选择其他目录或仓库名",
                path.display()
            )))
        }
        Err(error) => Err(DomainError::Other(format!(
            "创建克隆目标目录 {} 失败：{error}",
            path.display()
        ))),
    }
}

pub(super) fn remove_cancelled_clone_directory(path: &std::path::Path) -> Result<()> {
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
            "路径已不再是本次克隆创建的普通目录，已拒绝删除：{}",
            path.display()
        )));
    }

    let git_path = path.join(".git");
    match std::fs::symlink_metadata(&git_path) {
        Ok(git_metadata) if git_metadata.is_dir() && !git_metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|error| {
                DomainError::Other(format!("删除残留克隆目录 {} 失败：{error}", path.display()))
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
                        "删除空的克隆目标目录 {} 失败：{error}",
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
