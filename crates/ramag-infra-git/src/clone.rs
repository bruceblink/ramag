//! Clone / Init 操作（subprocess git）

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    run_git_bytes, run_git_streaming, validate_path_arg, validate_positional_arg,
};

/// Clone 远程仓库到 `dest` 目录（由 git 自动创建）
pub fn clone_repo(url: &str, dest: &Path) -> Result<()> {
    validate_positional_arg(url, "仓库 URL")?;
    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    validate_path_arg(dest_str, "Clone 目标路径")?;
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
    validate_positional_arg(url, "仓库 URL")?;
    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    validate_path_arg(dest_str, "Clone 目标路径")?;
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
    validate_path_arg(path_str, "初始化目标路径")?;
    run_git_bytes(
        path.parent().unwrap_or(Path::new(".")),
        &["init", "--", path_str],
    )
    .map(|_| ())
}
