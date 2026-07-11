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
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new("git");
        command.env("GIT_TERMINAL_PROMPT", "0");
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
    }
}
