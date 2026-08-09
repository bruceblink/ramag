//! `git` 子进程封装。机器可读查询与写操作统一走系统 Git；gix 只负责仓库发现。
mod validation;

pub(crate) use validation::*;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

use ramag_domain::entities::{
    MAX_GIT_NAME_ARG_BYTES, MAX_GIT_PATCH_BYTES, MAX_GIT_PATH_ARGS, MAX_GIT_PATH_ARGS_BYTES,
    MAX_GIT_PATH_BYTES, MAX_GIT_PATH_DEPTH, MAX_GIT_POSITIONAL_ARG_BYTES,
};
use ramag_domain::error::{DomainError, Result};
use tracing::{debug, info, warn};

use crate::errors::friendly_git_error;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROGRESS_LINE_MAX_BYTES: usize = 16 * 1024;
pub(crate) const MAX_STDOUT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_STDERR_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PARSED_GIT_ITEMS: usize = 250_000;
pub(crate) const MAX_GIT_RECORD_BYTES: usize = 64 * 1024;
pub(crate) const MAX_GIT_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// GUI 进程在 Windows 启动 git.exe 时禁止创建闪烁的控制台窗口。
pub(crate) fn command() -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("git");
        command.creation_flags(CREATE_NO_WINDOW);
        // GUI 子进程没有可用终端；仍允许 Git Credential Manager 等图形凭据助手运行。
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_EDITOR", "true");
        command.env("GIT_LITERAL_PATHSPECS", "1");
        command.env("GIT_OPTIONAL_LOCKS", "0");
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new("git");
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GIT_EDITOR", "true");
        command.env("GIT_LITERAL_PATHSPECS", "1");
        command.env("GIT_OPTIONAL_LOCKS", "0");
        command
    }
}

/// `-C` 锁定仓库目录；`-c core.quotepath=false` 让非 ASCII 路径走原始 utf-8
pub fn run_git_bytes(repo_path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let started = std::time::Instant::now();
    let output = run_git_output(repo_path, args)?;
    if !output.status.success() {
        let error = output_error(args, &output);
        warn!(
            error = %error,
            operation = args.first().copied().unwrap_or("unknown"),
            exit_code = ?output.status.code(),
            elapsed_ms = started.elapsed().as_millis(),
            "git command failed"
        );
        return Err(error);
    }
    debug!(
        repo = %repo_path.display(),
        operation = args.first().copied().unwrap_or("unknown"),
        bytes = output.stdout.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "git command completed"
    );
    Ok(output.stdout)
}

/// 引用存在性探测：成功为 true，Git 约定的退出码 1 为 false，其它失败必须上报。
pub(crate) fn run_git_probe(repo_path: &Path, args: &[&str]) -> Result<bool> {
    let output = run_git_output(repo_path, args)?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(output_error(args, &output))
}

fn run_git_output(repo_path: &Path, args: &[&str]) -> Result<Output> {
    debug!(
        repo = %repo_path.display(),
        operation = args.first().copied().unwrap_or("unknown"),
        arg_count = args.len(),
        "git subprocess"
    );
    let mut git = machine_command(repo_path);
    git.args(args);
    run_command_output_limited(git, args.first().copied().unwrap_or("unknown"))
}

/// 构造机器可读查询命令；流式分页与一次性查询必须复用完全相同的环境和 Git 配置。
pub(crate) fn machine_command(repo_path: &Path) -> Command {
    let mut git = command();
    git
        // 固定机器可读输出，避免 Windows 本地化 Git 让错误和 ahead/behind 解析失效。
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("--no-pager")
        // status 不执行仓库配置的 fsmonitor hook；签名文本也不得污染格式化输出。
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("log.showSignature=false")
        .arg("-c")
        .arg("core.quotepath=false")
        // Git for Windows 默认可能受 MAX_PATH 限制；该配置只影响 Windows 实现，其它平台无副作用。
        .arg("-c")
        .arg("core.longpaths=true")
        .arg("-C")
        .arg(repo_path);
    git
}

pub(crate) fn run_command_output_limited(mut command: Command, operation: &str) -> Result<Output> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "git {operation} 调用失败（请确认已安装 git）：{error}"
            ))
        })?;
    wait_with_output_limited(child, operation)
}

fn wait_with_output_limited(mut child: Child, operation: &str) -> Result<Output> {
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |error| format!("；清理失败：{error}"));
            return Err(DomainError::QueryFailed(format!(
                "无法读取 git {operation} 标准输出{cleanup}"
            )));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |error| format!("；清理失败：{error}"));
            return Err(DomainError::QueryFailed(format!(
                "无法读取 git {operation} 错误输出{cleanup}"
            )));
        }
    };

    // stdout 与 stderr 必须并发排空，否则任一管道写满都会让子进程互相等待。
    let stdout_reader = match std::thread::Builder::new()
        .name("ramag-git-stdout".into())
        .spawn(move || read_limited(stdout, MAX_STDOUT_BYTES))
    {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |cleanup| format!("；清理失败：{cleanup}"));
            return Err(DomainError::QueryFailed(format!(
                "启动 git {operation} 输出读取线程失败：{error}{cleanup}"
            )));
        }
    };
    let stderr_result = read_limited(stderr, MAX_STDERR_BYTES);
    let status = match wait_child_or_cleanup(&mut child, operation) {
        Ok(status) => status,
        Err(error) => {
            // 清理失败时也不等待读取线程，避免异常进程继续持有管道导致当前线程永久阻塞。
            drop(stdout_reader);
            return Err(error);
        }
    };
    let stdout_result = stdout_reader.join();

    let mut stderr = stderr_result.map_err(|error| {
        DomainError::QueryFailed(format!("读取 git {operation} 错误输出失败：{error}"))
    })?;
    let stdout = stdout_result
        .map_err(|_| DomainError::QueryFailed(format!("git {operation} 输出读取线程 panic")))?
        .map_err(|error| {
            DomainError::QueryFailed(format!("读取 git {operation} 标准输出失败：{error}"))
        })?;

    if stdout.truncated {
        return Err(DomainError::QueryFailed(format!(
            "git {operation} 输出超过 {} MiB 安全上限，请缩小操作范围",
            MAX_STDOUT_BYTES / 1024 / 1024
        )));
    }
    if stderr.truncated {
        stderr
            .bytes
            .extend_from_slice(b"\n... git stderr truncated by Ramag");
    }
    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

pub(crate) struct LimitedBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

pub(crate) fn read_limited(
    mut reader: impl std::io::Read,
    limit: usize,
) -> std::io::Result<LimitedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let keep = read.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..keep]);
        if keep < read {
            truncated = true;
        }
    }
    Ok(LimitedBytes { bytes, truncated })
}

fn output_error(args: &[&str], output: &Output) -> DomainError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    DomainError::QueryFailed(friendly_git_error(args, &stderr))
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

    let operation = args.first().copied().unwrap_or("unknown");
    let started = std::time::Instant::now();
    info!(operation, "git streaming command started");
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
        let watcher_child = child.clone();
        let cancel = cancel.clone();
        match std::thread::Builder::new()
            .name("ramag-git-cancel".into())
            .spawn(move || -> Result<()> {
                loop {
                    let mut child = watcher_child
                        .lock()
                        .map_err(|_| DomainError::QueryFailed("git 子进程状态锁已损坏".into()))?;
                    if cancel.load(Ordering::Relaxed) {
                        if child
                            .try_wait()
                            .map_err(|e| {
                                DomainError::QueryFailed(format!("检查 git 状态失败: {e}"))
                            })?
                            .is_none()
                        {
                            child.kill().map_err(|e| {
                                DomainError::QueryFailed(format!("取消 git 进程失败: {e}"))
                            })?;
                        }
                        return Ok(());
                    }
                    if child
                        .try_wait()
                        .map_err(|e| DomainError::QueryFailed(format!("检查 git 状态失败: {e}")))?
                        .is_some()
                    {
                        return Ok(());
                    }
                    drop(child);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }) {
            Ok(watcher) => watcher,
            Err(e) => {
                drop(stderr);
                let cleanup = child
                    .lock()
                    .map_err(|_| DomainError::QueryFailed("git 子进程状态锁已损坏".into()))
                    .and_then(|mut child| terminate_child(&mut child));
                let detail = cleanup
                    .err()
                    .map_or_else(String::new, |error| format!("；清理失败：{error}"));
                return Err(DomainError::QueryFailed(format!(
                    "启动 git 取消监控线程失败: {e}{detail}"
                )));
            }
        }
    };

    // 逐段读 stderr：git 进度用 \r 原地刷新，按 \r / \n 切行取最新一行写入共享槽
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut line_truncated = false;
    let mut last_lines = std::collections::VecDeque::with_capacity(8);
    let mut read_error = None;
    loop {
        let n = match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                read_error = Some(e);
                cancel.store(true, Ordering::Relaxed);
                break;
            }
        };
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                record_progress_line(&mut line, &mut line_truncated, &progress, &mut last_lines);
            } else if line.len() < PROGRESS_LINE_MAX_BYTES {
                line.push(b);
            } else {
                line_truncated = true;
            }
        }
    }
    record_progress_line(&mut line, &mut line_truncated, &progress, &mut last_lines);
    drop(stderr);

    let status_result = match child.lock() {
        Ok(mut child) => child
            .wait()
            .map_err(|e| DomainError::QueryFailed(format!("git 等待失败: {e}"))),
        Err(_) => Err(DomainError::QueryFailed("git 子进程状态不可用".into())),
    };
    let watcher_result = watcher
        .join()
        .map_err(|_| DomainError::QueryFailed("git 取消监控线程发生 panic".into()))?;

    if let Some(e) = read_error {
        return Err(DomainError::QueryFailed(format!("读取 git 进度失败: {e}")));
    }
    watcher_result?;
    let status = status_result?;

    if cancel.load(Ordering::Relaxed) {
        info!(
            operation,
            elapsed_ms = started.elapsed().as_millis(),
            "git streaming command cancelled"
        );
        return Err(DomainError::QueryFailed("已取消操作".into()));
    }
    if !status.success() {
        let tail: Vec<String> = last_lines.into_iter().collect();
        let error = DomainError::QueryFailed(friendly_git_error(args, &tail.join("\n")));
        warn!(error = %error, operation, exit_code = ?status.code(), elapsed_ms = started.elapsed().as_millis(), "git streaming command failed");
        return Err(error);
    }
    info!(
        operation,
        elapsed_ms = started.elapsed().as_millis(),
        "git streaming command completed"
    );
    Ok(())
}

/// 把文本写入 stdin。此类命令不消费 stdout；stderr 在写入期间并发排空，避免钩子输出堵塞。
pub fn run_git_stdin(repo_path: &Path, args: &[&str], stdin_text: &str) -> Result<()> {
    use std::io::Write;
    if stdin_text.len() > MAX_GIT_PATCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "git stdin 输入超过 {} MiB 安全上限",
            MAX_GIT_PATCH_BYTES / 1024 / 1024
        )));
    }
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
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DomainError::QueryFailed(format!("git 调用失败: {e}")))?;
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |error| format!("；清理失败：{error}"));
            return Err(DomainError::QueryFailed(format!(
                "无法打开 git stdin{cleanup}"
            )));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdin);
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |error| format!("；清理失败：{error}"));
            return Err(DomainError::QueryFailed(format!(
                "无法读取 git stderr{cleanup}"
            )));
        }
    };
    let stderr_reader = match std::thread::Builder::new()
        .name("ramag-git-stderr".into())
        .spawn(move || read_limited(stderr, MAX_STDERR_BYTES))
    {
        Ok(reader) => reader,
        Err(error) => {
            drop(stdin);
            let cleanup = terminate_child(&mut child)
                .err()
                .map_or_else(String::new, |cleanup| format!("；清理失败：{cleanup}"));
            return Err(DomainError::QueryFailed(format!(
                "启动 git stderr 读取线程失败：{error}{cleanup}"
            )));
        }
    };
    if let Err(e) = stdin.write_all(stdin_text.as_bytes()) {
        drop(stdin);
        let cleanup = terminate_child(&mut child)
            .err()
            .map_or_else(String::new, |error| format!("；清理失败：{error}"));
        if stderr_reader.join().is_err() {
            tracing::warn!("git stderr reader panicked during cleanup");
        }
        return Err(DomainError::QueryFailed(format!(
            "写入 git stdin 失败: {e}{cleanup}"
        )));
    }
    drop(stdin);
    let operation = args.first().copied().unwrap_or("unknown");
    let status = match wait_child_or_cleanup(&mut child, operation) {
        Ok(status) => status,
        Err(error) => {
            drop(stderr_reader);
            return Err(error);
        }
    };
    let stderr_result = stderr_reader.join();
    let mut stderr = stderr_result
        .map_err(|_| DomainError::QueryFailed("git stderr 读取线程 panic".into()))?
        .map_err(|error| DomainError::QueryFailed(format!("读取 git stderr 失败：{error}")))?;
    if stderr.truncated {
        stderr
            .bytes
            .extend_from_slice(b"\n... git stderr truncated by Ramag");
    }
    if !status.success() {
        let err = String::from_utf8_lossy(&stderr.bytes);
        return Err(DomainError::QueryFailed(friendly_git_error(args, &err)));
    }
    Ok(())
}

/// 路径列表走 NUL 分隔 stdin，避开系统 argv 总长度上限并保留空白、换行等合法文件名。
pub(crate) fn run_git_pathspecs(repo_path: &Path, args: &[&str], paths: &[String]) -> Result<()> {
    let encoded = encode_pathspecs(paths)?;
    run_git_stdin(repo_path, args, &encoded)
}

fn record_progress_line(
    line: &mut Vec<u8>,
    truncated: &mut bool,
    progress: &std::sync::Mutex<String>,
    last_lines: &mut std::collections::VecDeque<String>,
) {
    if line.is_empty() {
        *truncated = false;
        return;
    }
    let mut text = String::from_utf8_lossy(line).into_owned();
    if *truncated {
        text.push_str(" …");
    }
    match progress.lock() {
        Ok(mut slot) => *slot = text.clone(),
        Err(_) => tracing::warn!("git progress lock poisoned"),
    }
    if last_lines.len() == 8 {
        last_lines.pop_front();
    }
    last_lines.push_back(text);
    line.clear();
    *truncated = false;
}

fn terminate_child(child: &mut std::process::Child) -> Result<()> {
    let running = child
        .try_wait()
        .map_err(|e| DomainError::QueryFailed(format!("检查 git 进程失败: {e}")))?
        .is_none();
    if running {
        child
            .kill()
            .map_err(|e| DomainError::QueryFailed(format!("终止 git 进程失败: {e}")))?;
    }
    child
        .wait()
        .map_err(|e| DomainError::QueryFailed(format!("回收 git 进程失败: {e}")))?;
    Ok(())
}

fn wait_child_or_cleanup(
    child: &mut std::process::Child,
    operation: &str,
) -> Result<std::process::ExitStatus> {
    match child.wait() {
        Ok(status) => Ok(status),
        Err(error) => {
            // wait 异常时不能再假设子进程已退出；尽力终止并回收，且不覆盖原始错误。
            let kill_error = child.kill().err();
            let reap_error = child.wait().err();
            let cleanup = match (kill_error, reap_error) {
                (None, None) => String::new(),
                (kill, reap) => format!(
                    "；清理失败：kill={}，wait={}",
                    kill.map_or_else(|| "ok".into(), |error| error.to_string()),
                    reap.map_or_else(|| "ok".into(), |error| error.to_string())
                ),
            };
            Err(DomainError::QueryFailed(format!(
                "等待 git {operation} 进程失败：{error}{cleanup}"
            )))
        }
    }
}

#[cfg(test)]
mod tests;
