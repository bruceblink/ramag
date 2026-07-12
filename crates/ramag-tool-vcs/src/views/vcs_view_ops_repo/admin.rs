//! 仓库 storage 管理 + Clone / Init / 确认弹窗

use gpui::{Context, Window};
use ramag_domain::entities::{RepoConfig, RepoId};

use super::super::vcs_view::VcsView;
use super::open_repo_async;

/// 打开中的仓库 Tab 路径列表的偏好 key（JSON 数组，跨重启恢复用）
const OPEN_REPOS_PREF: &str = "vcs_open_repos";

impl VcsView {
    /// 收藏 / 取消收藏最近仓库，并立即持久化。
    pub(crate) fn toggle_repo_favorite(&mut self, path: String, cx: &mut Context<Self>) {
        let repo = self
            .recent_repos
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
        let driver = self.driver.clone();
        self.loading = true;
        let repo_hint = url
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .unwrap_or("仓库")
            .trim_end_matches(".git");
        self.loading_label = Some(format!(
            "正在 Clone {repo_hint} 到 {}…（大仓库可能需要几分钟）",
            dest.display()
        ));
        self.error = None;
        self.show_clone_panel = false;
        cx.notify();
        cx.spawn(
            async move |this, cx| match driver.clone_repo(&url, &dest).await {
                Ok(rc) => {
                    tracing::info!("vcs clone done");
                    open_repo_async(&this, driver, std::path::PathBuf::from(&rc.path), cx).await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "vcs: clone failed");
                    let _ = this.update(cx, |this, cx| {
                        this.loading = false;
                        this.loading_label = None;
                        this.error = Some(format!("Clone 失败: {e}"));
                        cx.notify();
                    });
                }
            },
        )
        .detach();
    }

    /// 异步初始化空仓库（真正执行 git init），完成后打开 session。
    /// 目录已是 git 仓库时 init 幂等无害（git init 对既有仓库安全）
    pub(crate) fn init_repo_async(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
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
            let open_paths: Vec<String> = match storage.get_preference(OPEN_REPOS_PREF).await {
                Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
                _ => Vec::new(),
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(list) => {
                    // 按保存顺序恢复 Tab；已从 recent 移除的仓库自动跳过
                    this.open_repos = open_paths
                        .iter()
                        .filter_map(|p| list.iter().find(|r| &r.path == p).cloned())
                        .collect();
                    this.recent_repos = list;
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

    /// 把当前打开的仓库 Tab 路径列表落 prefs（开 / 关 Tab 时调；后台异步，失败仅日志）
    pub(crate) fn persist_open_repos(&self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.open_repos.iter().map(|r| r.path.clone()).collect();
        let json = match serde_json::to_string(&paths) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "vcs: serialize open repos failed");
                return;
            }
        };
        let storage = self.storage.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = storage.set_preference(OPEN_REPOS_PREF, &json).await {
                    tracing::warn!(error = %e, "vcs: persist open repos failed");
                }
            })
            .detach();
    }
}
