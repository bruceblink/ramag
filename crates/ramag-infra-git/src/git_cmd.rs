//! `git` 子进程封装。写操作 + 复杂查询走 subprocess（凭证 / 钩子由系统 git 处理）；
//! 读元数据 / 分支 / log 走 gix（更快）

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use ramag_domain::error::{DomainError, Result};
use tracing::debug;

use crate::errors::friendly_git_error;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// GUI 进程在 Windows 启动 git.exe 时禁止创建闪烁的控制台窗口。
pub(crate) fn command() -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("git");
        command.creation_flags(CREATE_NO_WINDOW);
        // GUI 子进程没有可用终端；仍允许 Git Credential Manager 等图形凭据助手运行。
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_EDITOR", "true");
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new("git");
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_EDITOR", "true");
        command
    }
}

/// `-C` 锁定仓库目录；`-c core.quotepath=false` 让非 ASCII 路径走原始 utf-8
pub fn run_git_bytes(repo_path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    debug!(
        operation = args.first().copied().unwrap_or("unknown"),
        arg_count = args.len(),
        "git subprocess"
    );
    let output = command()
        // 固定机器可读输出，避免 Windows 本地化 Git 让错误和 ahead/behind 解析失效。
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("-c")
        .arg("core.quotepath=false")
        // Git for Windows 默认可能受 MAX_PATH 限制；该配置只影响 Windows 实现，其它平台无副作用。
        .arg("-c")
        .arg("core.longpaths=true")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .map_err(|e| DomainError::QueryFailed(format!("git 调用失败（请确认已安装 git）: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(DomainError::QueryFailed(friendly_git_error(args, &err)));
    }
    Ok(output.stdout)
}

/// stdout 解析成 String，非 UTF-8 走 lossy
pub fn run_git_text(repo_path: &Path, args: &[&str]) -> Result<String> {
    let bytes = run_git_bytes(repo_path, args)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 流式执行带进度的 git 命令（`--progress` 走 stderr）：`git -C <dir> <args>`。
/// 进度行（\r / \n 分隔）持续写入 `progress` 共享槽供 UI 每帧读取；`cancel` 置位后
/// watcher 线程 kill 子进程。clone / fetch / pull / push 共用同一实现。
/// 调用方须在 args 内自带 `--progress`（及子命令名）
pub(crate) fn run_git_streaming(
    dir: &Path,
    args: &[&str],
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: std::sync::Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    let child = command()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("-C")
        .arg(dir)
        .args(args)
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
        return Err(DomainError::QueryFailed("已取消操作".into()));
    }
    if !status.success() {
        let tail: Vec<String> = last_lines.into_iter().collect();
        return Err(DomainError::QueryFailed(friendly_git_error(
            args,
            &tail.join("\n"),
        )));
    }
    Ok(())
}

/// 把文本写入 stdin。`git apply --cached` / `git am` 等用
pub fn run_git_stdin(repo_path: &Path, args: &[&str], stdin_text: &str) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;
    debug!(
        operation = args.first().copied().unwrap_or("unknown"),
        arg_count = args.len(),
        stdin_len = stdin_text.len(),
        "git subprocess (stdin)"
    );
    let mut child = command()
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("-c")
        .arg("core.quotepath=false")
        .arg("-c")
        .arg("core.longpaths=true")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DomainError::QueryFailed(format!("git 调用失败: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_text.as_bytes())
            .map_err(|e| DomainError::QueryFailed(format!("写入 git stdin 失败: {e}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| DomainError::QueryFailed(format!("git 等待失败: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(DomainError::QueryFailed(friendly_git_error(args, &err)));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::command;

    #[test]
    fn git_terminal_prompt_is_disabled_for_gui_processes() {
        let command = command();
        let prompt = command
            .get_envs()
            .find(|(key, _)| *key == "GIT_TERMINAL_PROMPT")
            .and_then(|(_, value)| value);
        assert_eq!(prompt, Some(std::ffi::OsStr::new("0")));
        let editor = command
            .get_envs()
            .find(|(key, _)| *key == "GIT_EDITOR")
            .and_then(|(_, value)| value);
        assert_eq!(editor, Some(std::ffi::OsStr::new("true")));
    }
}
