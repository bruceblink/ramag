//! 仓库文件系统监听：外部改动（编辑器保存 / 终端 git 操作）→ 过滤 + 防抖 → 通知刷新。
//! 配合 refresh_workspace_silent 的 status 指纹比对，无实质变化时 UI 零扰动

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// 事件静默 800ms 后才触发回调（尾沿防抖）：编辑器批量保存 / git 写库的连发事件合并为一次
const DEBOUNCE: Duration = Duration::from_millis(800);

/// 监听句柄：drop 即停止监听，防抖线程随通道关闭自动退出
pub(crate) struct RepoWatcher {
    watcher: Option<RecommendedWatcher>,
    debounce_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RepoWatcher {
    fn drop(&mut self) {
        // 先释放 notify watcher，使其回调中的 sender 关闭，再等待防抖线程退出。
        self.watcher.take();
        if let Some(thread) = self.debounce_thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("vcs fs debounce thread panicked");
        }
    }
}

impl RepoWatcher {
    /// 递归监听 repo_root；`on_change` 在防抖线程上调用（调用方负责切回 UI 线程）
    pub(crate) fn start(
        repo_root: PathBuf,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
        // 容量 1 即可表达“有变更待处理”；事件风暴中的重复信号直接合并，避免无界排队。
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let root_for_filter = repo_root.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let event = match res {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(error = %error, "vcs fs watcher event failed");
                        return;
                    }
                };
                if event.paths.iter().any(|p| is_relevant(&root_for_filter, p)) {
                    enqueue_change(&tx);
                }
            })?;
        watcher.watch(&repo_root, RecursiveMode::Recursive)?;

        let debounce_thread = std::thread::Builder::new()
            .name("ramag-vcs-fs-debounce".into())
            .spawn(move || {
                // 首个事件到达后，持续吸收事件直到静默满 DEBOUNCE，合并为一次回调
                while rx.recv().is_ok() {
                    loop {
                        match rx.recv_timeout(DEBOUNCE) {
                            Ok(()) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            // watcher 已 drop：线程退出
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    on_change();
                }
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher: Some(watcher),
            debounce_thread: Some(debounce_thread),
        })
    }
}

fn enqueue_change(sender: &mpsc::SyncSender<()>) {
    match sender.try_send(()) {
        Ok(()) | Err(mpsc::TrySendError::Full(())) => {}
        Err(mpsc::TrySendError::Disconnected(())) => {
            // watcher 正在释放，接收端已退出，无需上报噪声。
        }
    }
}

/// 事件过滤：工作区文件一律相关；`.git` 内部只放行表示仓库状态变化的关键路径，
/// 屏蔽 objects / logs / COMMIT_EDITMSG / *.lock 等高频噪声
fn is_relevant(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy());
    let Some(first) = comps.next() else {
        return false;
    };
    if first != ".git" {
        return true;
    }
    let Some(second) = comps.next() else {
        return false;
    };
    matches!(
        second.as_ref(),
        // index 变化 = stage/unstage/commit；HEAD/refs = 分支移动；其余 = 进行中操作标记
        "index"
            | "HEAD"
            | "ORIG_HEAD"
            | "MERGE_HEAD"
            | "CHERRY_PICK_HEAD"
            | "REVERT_HEAD"
            | "refs"
            | "rebase-merge"
            | "rebase-apply"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(p: &str) -> bool {
        is_relevant(Path::new("/repo"), Path::new(p))
    }

    #[test]
    fn worktree_files_are_relevant() {
        assert!(rel("/repo/src/main.rs"));
        assert!(rel("/repo/README.md"));
    }

    #[test]
    fn git_state_files_are_relevant() {
        assert!(rel("/repo/.git/index"));
        assert!(rel("/repo/.git/HEAD"));
        assert!(rel("/repo/.git/refs/heads/main"));
        assert!(rel("/repo/.git/MERGE_HEAD"));
    }

    #[test]
    fn git_noise_is_filtered() {
        assert!(!rel("/repo/.git/objects/ab/cdef123"));
        assert!(!rel("/repo/.git/logs/HEAD"));
        assert!(!rel("/repo/.git/COMMIT_EDITMSG"));
        assert!(!rel("/repo/.git/index.lock"));
        assert!(!rel("/repo/.git/config"));
    }

    #[test]
    fn duplicate_change_signals_are_coalesced() {
        let (tx, rx) = mpsc::sync_channel(1);
        enqueue_change(&tx);
        enqueue_change(&tx);

        assert_eq!(rx.try_iter().count(), 1);
    }
}
