//! Clone / Init 操作（subprocess git）

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{run_git_bytes, run_git_streaming};

/// Clone 远程仓库到 `dest` 目录（由 git 自动创建）
pub fn clone_repo(url: &str, dest: &Path) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    run_git_bytes(
        dest.parent().unwrap_or(Path::new(".")),
        &["clone", "--", url, dest_str],
    )
    .map(|_| ())
}

/// Clone（带进度 + 可取消）：`git clone --progress` 的 stderr 进度行（\r / \n 分隔）
/// 持续写入 `progress` 共享槽供 UI 每帧读取；`cancel` 置位后 watcher 线程 kill 子进程。
/// 主线程阻塞读 stderr 至 EOF（kill 后管道关闭自然退出），不忙轮询
pub fn clone_repo_streaming(
    url: &str,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    // clone 在父目录内运行（目标目录由 git 创建）；共用流式执行器
    let dir = dest.parent().unwrap_or(Path::new("."));
    run_git_streaming(
        dir,
        &["clone", "--progress", "--", url, dest_str],
        cancel,
        progress,
    )
}

/// 在 `path` 目录初始化新 git 仓库（`git init`）
pub fn init_repo(path: &Path) -> Result<()> {
    let path_str = path
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    // git init 在目标目录内运行（不存在则自动创建）
    run_git_bytes(
        path.parent().unwrap_or(Path::new(".")),
        &["init", "--", path_str],
    )
    .map(|_| ())
}
