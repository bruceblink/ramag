//! Clone / Init 操作（subprocess git）

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ramag_domain::error::{DomainError, Result};

use crate::errors::friendly_git_error;
use crate::git_cmd::run_git_bytes;

/// Clone 远程仓库到 `dest` 目录（由 git 自动创建）
pub fn clone_repo(url: &str, dest: &Path) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    run_git_bytes(
        dest.parent().unwrap_or(Path::new(".")),
        &["clone", url, dest_str],
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
    use std::io::Read;
    use std::process::Stdio;

    let dest_str = dest
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    let child = crate::git_cmd::command()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("-C")
        .arg(dest.parent().unwrap_or(Path::new(".")))
        .args(["clone", "--progress", url, dest_str])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DomainError::QueryFailed(format!("git 调用失败（请确认已安装 git）: {e}")))?;

    let child = Arc::new(Mutex::new(child));
    let mut stderr = match child.lock() {
        Ok(mut c) => c.stderr.take(),
        Err(_) => None,
    }
    .ok_or_else(|| DomainError::QueryFailed("无法读取 git 输出".into()))?;

    // watcher：轮询取消位与子进程退出；取消即 kill（stderr 管道随之关闭，主读循环结束）
    let watcher = {
        let child = child.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    break;
                }
                if let Ok(mut c) = child.lock()
                    && matches!(c.try_wait(), Ok(Some(_)))
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        })
    };

    // 逐段读 stderr：git 进度用 \r 原地刷新，按 \r / \n 切行取最新一行写入共享槽
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut last_lines = std::collections::VecDeque::with_capacity(8);
    loop {
        let n = match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                if !line.is_empty() {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    if let Ok(mut slot) = progress.lock() {
                        *slot = text.clone();
                    }
                    if last_lines.len() == 8 {
                        last_lines.pop_front();
                    }
                    last_lines.push_back(text);
                    line.clear();
                }
            } else {
                line.push(b);
            }
        }
    }

    let status = match child.lock() {
        Ok(mut c) => c
            .wait()
            .map_err(|e| DomainError::QueryFailed(format!("git 等待失败: {e}")))?,
        Err(_) => return Err(DomainError::QueryFailed("git 子进程状态不可用".into())),
    };
    let _ = watcher.join();

    if cancel.load(Ordering::Relaxed) {
        return Err(DomainError::QueryFailed("已取消 Clone".into()));
    }
    if !status.success() {
        let tail: Vec<String> = last_lines.into_iter().collect();
        return Err(DomainError::QueryFailed(friendly_git_error(
            &["clone"],
            &tail.join("\n"),
        )));
    }
    Ok(())
}

/// 在 `path` 目录初始化新 git 仓库（`git init`）
pub fn init_repo(path: &Path) -> Result<()> {
    let path_str = path
        .to_str()
        .ok_or_else(|| DomainError::InvalidConfig("目标路径含非 UTF-8 字符".into()))?;
    // git init 在目标目录内运行（不存在则自动创建）
    run_git_bytes(path.parent().unwrap_or(Path::new(".")), &["init", path_str]).map(|_| ())
}
