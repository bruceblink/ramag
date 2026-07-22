//! 仓库文件系统监听：按路径增量刷新工作区，Git 元数据变化才刷新完整状态与分支。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use ramag_domain::entities::{MAX_INCREMENTAL_STATUS_PATH_BYTES, MAX_INCREMENTAL_STATUS_PATHS};

/// 单次文件保存通常在数毫秒内产生一组事件；安静一个短窗口后立即刷新。
const DEBOUNCE_QUIET: Duration = Duration::from_millis(12);
/// 持续事件流也必须定期交付，避免尾缘防抖无限延后界面状态。
const DEBOUNCE_MAX: Duration = Duration::from_millis(40);
const MAX_GIT_POINTER_FILE_BYTES: u64 = 4 * 1024;

/// 一批文件系统事件对应的最小刷新范围。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RepoRefresh {
    pub(crate) full_status: bool,
    pub(crate) refresh_refs: bool,
    pub(crate) paths: Vec<String>,
}

impl RepoRefresh {
    pub(crate) fn full() -> Self {
        Self {
            full_status: true,
            refresh_refs: true,
            paths: Vec::new(),
        }
    }

    fn status_full() -> Self {
        Self {
            full_status: true,
            refresh_refs: false,
            paths: Vec::new(),
        }
    }

    fn refs() -> Self {
        Self::full()
    }

    fn path(path: String) -> Self {
        Self {
            paths: vec![path],
            ..Self::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.full_status && !self.refresh_refs && self.paths.is_empty()
    }

    /// 合并刷新范围；路径批次超出跨平台 argv 安全界限时退化为一次完整 status。
    pub(crate) fn merge(&mut self, other: Self) {
        self.refresh_refs |= other.refresh_refs;
        self.full_status |= other.full_status || self.refresh_refs;
        if self.full_status {
            self.paths.clear();
            return;
        }
        self.paths.extend(other.paths);
        coalesce_paths(&mut self.paths);
        let bytes = self
            .paths
            .iter()
            .try_fold(0usize, |total, path| total.checked_add(path.len() + 1));
        if self.paths.len() > MAX_INCREMENTAL_STATUS_PATHS
            || bytes.is_none_or(|bytes| bytes > MAX_INCREMENTAL_STATUS_PATH_BYTES)
        {
            *self = Self::status_full();
        }
    }
}

/// 监听句柄：drop 即停止监听，防抖线程随通道关闭自动退出。
pub(crate) struct RepoWatcher {
    watcher: Option<RecommendedWatcher>,
    debounce_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        self.watcher.take();
        if let Some(thread) = self.debounce_thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("vcs fs debounce thread panicked");
        }
    }
}

impl RepoWatcher {
    /// 递归监听 repo_root；回调携带本批事件的最小刷新范围。
    pub(crate) fn start(
        repo_root: PathBuf,
        on_change: impl Fn(RepoRefresh) + Send + 'static,
    ) -> notify::Result<Self> {
        let metadata_roots = resolve_git_metadata_roots(&repo_root);
        // 通道只表达“有事件”；具体范围在共享槽合并，满载时也不会丢 Git 元数据变化。
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let pending = Arc::new(Mutex::new(RepoRefresh::default()));
        let pending_for_watcher = pending.clone();
        let root_for_filter = repo_root.clone();
        let metadata_roots_for_filter = metadata_roots.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let event = match result {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(error = %error, "vcs fs watcher event failed");
                        return;
                    }
                };
                let change = classify_event(&root_for_filter, &metadata_roots_for_filter, &event);
                if change.is_empty() {
                    return;
                }
                merge_pending(&pending_for_watcher, change);
                enqueue_change(&tx);
            })?;
        watcher.watch(&repo_root, RecursiveMode::Recursive)?;
        for metadata_root in metadata_roots {
            if metadata_root.starts_with(&repo_root) {
                continue;
            }
            if let Err(error) = watcher.watch(&metadata_root, RecursiveMode::Recursive) {
                tracing::warn!(
                    error = %error,
                    path = %metadata_root.display(),
                    "vcs linked-worktree metadata watch failed"
                );
            }
        }

        let debounce_thread = std::thread::Builder::new()
            .name("ramag-vcs-fs-debounce".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    let deadline = Instant::now() + DEBOUNCE_MAX;
                    loop {
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let timeout = DEBOUNCE_QUIET.min(deadline.saturating_duration_since(now));
                        match rx.recv_timeout(timeout) {
                            Ok(()) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let change = take_pending(&pending);
                    if !change.is_empty() {
                        on_change(change);
                    }
                }
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher: Some(watcher),
            debounce_thread: Some(debounce_thread),
        })
    }
}

fn classify_event(root: &Path, metadata_roots: &[PathBuf], event: &notify::Event) -> RepoRefresh {
    if event.paths.is_empty() {
        return RepoRefresh::status_full();
    }
    let mut refresh = RepoRefresh::default();
    for path in &event.paths {
        refresh.merge(classify_path(root, metadata_roots, path));
    }
    refresh
}

fn classify_path(root: &Path, metadata_roots: &[PathBuf], path: &Path) -> RepoRefresh {
    // 自定义 gitdir 可能位于工作树内部，必须先按更具体的元数据根匹配。
    for metadata_root in metadata_roots {
        if let Ok(relative) = path.strip_prefix(metadata_root) {
            return classify_git_relative(relative);
        }
    }
    if let Ok(relative) = path.strip_prefix(root) {
        return classify_worktree_relative(relative);
    }
    RepoRefresh::full()
}

fn classify_worktree_relative(relative: &Path) -> RepoRefresh {
    let Some(relative) = relative.to_str() else {
        return RepoRefresh::status_full();
    };
    let relative = relative.replace('\\', "/");
    let relative = relative.trim_matches('/');
    if relative.is_empty() {
        return RepoRefresh::status_full();
    }
    if relative == ".git" {
        return RepoRefresh::default();
    }
    if let Some(git_relative) = relative.strip_prefix(".git/") {
        return classify_git_relative_str(git_relative);
    }
    classify_worktree_path(relative)
}

fn classify_git_relative(relative: &Path) -> RepoRefresh {
    let Some(relative) = relative.to_str() else {
        return RepoRefresh::status_full();
    };
    let relative = relative.replace('\\', "/");
    classify_git_relative_str(relative.trim_matches('/'))
}

fn classify_worktree_path(relative: &str) -> RepoRefresh {
    let (parent, name) = relative
        .rsplit_once('/')
        .map_or(("", relative), |(parent, name)| (parent, name));
    match name {
        // 忽略规则会改变同目录整棵子树的可见成员，不能只刷新规则文件本身。
        ".gitignore" => {
            if parent.is_empty() {
                RepoRefresh::status_full()
            } else {
                RepoRefresh::path(parent.to_string())
            }
        }
        _ => RepoRefresh::path(relative.to_string()),
    }
}

fn classify_git_relative_str(relative: &str) -> RepoRefresh {
    if relative == "info/exclude" {
        return RepoRefresh::status_full();
    }
    classify_git_component(relative.split('/').next())
}

fn classify_git_component(state_path: Option<&str>) -> RepoRefresh {
    let Some(state_path) = state_path else {
        return RepoRefresh::default();
    };
    match state_path {
        // 索引与进行中操作只影响 status；无需为 stage 等操作重扫全部分支。
        "index" | "ORIG_HEAD" | "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD"
        | "rebase-merge" | "rebase-apply" => RepoRefresh::status_full(),
        // HEAD、refs 与配置会改变当前分支、upstream 或远端分支列表。
        "HEAD" | "FETCH_HEAD" | "packed-refs" | "config" | "refs" => RepoRefresh::refs(),
        _ => RepoRefresh::default(),
    }
}

/// linked worktree 的 HEAD/index 位于外部 gitdir，refs/config 位于 commondir；两者都监听。
fn resolve_git_metadata_roots(repo_root: &Path) -> Vec<PathBuf> {
    let dot_git = repo_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let Some(value) = read_small_text(&dot_git) else {
            return Vec::new();
        };
        let Some(path) = value.trim().strip_prefix("gitdir:") else {
            tracing::warn!(path = %dot_git.display(), "vcs gitdir pointer is malformed");
            return Vec::new();
        };
        resolve_relative_path(repo_root, path.trim())
    };

    let mut roots = vec![git_dir.clone()];
    if let Some(common) = read_small_text(&git_dir.join("commondir")) {
        let common = resolve_relative_path(&git_dir, common.trim());
        if !roots.contains(&common) {
            roots.push(common);
        }
    }
    roots
}

fn read_small_text(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_POINTER_FILE_BYTES {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(error = %error, path = %path.display(), "vcs git metadata pointer read failed");
            None
        }
    }
}

fn resolve_relative_path(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    std::fs::canonicalize(&resolved).unwrap_or(resolved)
}

fn coalesce_paths(paths: &mut Vec<String>) {
    paths.sort_unstable();
    paths.dedup();
    let mut coalesced: Vec<String> = Vec::with_capacity(paths.len());
    let mut retained = HashSet::with_capacity(paths.len());
    for path in paths.drain(..) {
        let has_ancestor = path
            .match_indices('/')
            .any(|(separator, _)| retained.contains(&path[..separator]));
        if has_ancestor {
            continue;
        }
        retained.insert(path.clone());
        coalesced.push(path);
    }
    *paths = coalesced;
}

fn merge_pending(pending: &Mutex<RepoRefresh>, change: RepoRefresh) {
    match pending.lock() {
        Ok(mut pending) => pending.merge(change),
        Err(error) => {
            tracing::warn!("vcs fs pending change lock poisoned");
            error.into_inner().merge(change);
        }
    }
}

fn take_pending(pending: &Mutex<RepoRefresh>) -> RepoRefresh {
    match pending.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(error) => {
            tracing::warn!("vcs fs pending change lock poisoned");
            let mut pending = error.into_inner();
            std::mem::take(&mut *pending)
        }
    }
}

fn enqueue_change(sender: &mpsc::SyncSender<()>) {
    match sender.try_send(()) {
        Ok(()) | Err(mpsc::TrySendError::Full(())) | Err(mpsc::TrySendError::Disconnected(())) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_paths_are_incremental_and_coalesced() {
        let mut refresh = classify_path(Path::new("/repo"), &[], Path::new("/repo/src/lib.rs"));
        refresh.merge(classify_path(
            Path::new("/repo"),
            &[],
            Path::new("/repo/src/nested/mod.rs"),
        ));
        refresh.merge(RepoRefresh::path("src".into()));

        assert!(!refresh.full_status);
        assert!(!refresh.refresh_refs);
        assert_eq!(refresh.paths, ["src"]);
    }

    #[test]
    fn coalescing_keeps_ancestor_across_lexically_interleaved_sibling() {
        let mut paths = vec!["a/x".into(), "a-b".into(), "a".into()];

        coalesce_paths(&mut paths);

        assert_eq!(paths, ["a", "a-b"]);
    }

    #[test]
    fn git_state_requests_full_metadata_refresh() {
        let refresh = classify_path(Path::new("/repo"), &[], Path::new("/repo/.git/packed-refs"));

        assert!(refresh.full_status);
        assert!(refresh.refresh_refs);
        assert!(refresh.paths.is_empty());
    }

    #[test]
    fn git_index_refreshes_status_without_rescanning_branches() {
        let refresh = classify_path(Path::new("/repo"), &[], Path::new("/repo/.git/index"));

        assert!(refresh.full_status);
        assert!(!refresh.refresh_refs);
        assert!(refresh.paths.is_empty());
    }

    #[test]
    fn git_object_noise_is_ignored() {
        let refresh = classify_path(
            Path::new("/repo"),
            &[],
            Path::new("/repo/.git/objects/ab/cdef123"),
        );

        assert!(refresh.is_empty());
    }

    #[test]
    fn ignore_rules_refresh_the_scope_they_control() {
        let root = classify_path(Path::new("/repo"), &[], Path::new("/repo/.gitignore"));
        assert!(root.full_status);
        assert!(!root.refresh_refs);

        let nested = classify_path(
            Path::new("/repo"),
            &[],
            Path::new("/repo/crates/tool/.gitignore"),
        );
        assert!(!nested.full_status);
        assert_eq!(nested.paths, ["crates/tool"]);

        let exclude = classify_path(
            Path::new("/repo"),
            &[],
            Path::new("/repo/.git/info/exclude"),
        );
        assert!(exclude.full_status);
        assert!(!exclude.refresh_refs);
    }

    #[test]
    fn linked_worktree_metadata_is_classified_by_external_roots() {
        let roots = vec![
            PathBuf::from("/meta/worktrees/feature"),
            PathBuf::from("/meta"),
        ];

        let index = classify_path(
            Path::new("/worktree"),
            &roots,
            Path::new("/meta/worktrees/feature/index"),
        );
        assert!(index.full_status);
        assert!(!index.refresh_refs);

        let refs = classify_path(
            Path::new("/worktree"),
            &roots,
            Path::new("/meta/refs/heads/main"),
        );
        assert!(refs.full_status);
        assert!(refs.refresh_refs);

        let internal = classify_path(
            Path::new("/worktree"),
            &[PathBuf::from("/worktree/custom-gitdir")],
            Path::new("/worktree/custom-gitdir/index"),
        );
        assert!(internal.full_status);
        assert!(!internal.refresh_refs);
        assert!(internal.paths.is_empty());
    }

    #[test]
    fn too_many_paths_fall_back_to_one_full_status() {
        let mut refresh = RepoRefresh::default();
        for index in 0..=MAX_INCREMENTAL_STATUS_PATHS {
            refresh.merge(RepoRefresh::path(format!("src/{index}.rs")));
        }

        assert!(refresh.full_status);
        assert!(!refresh.refresh_refs);
        assert!(refresh.paths.is_empty());
    }
}
