//! `git commit`。subject 走 `-m` 避免编辑器弹出；成功后 `rev-parse HEAD` 取新 hash

use std::path::Path;

use ramag_domain::entities::{CommitId, MAX_COMMIT_MESSAGE_BYTES};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{run_git_bytes, run_git_stdin, run_git_text};

pub fn run(repo_path: &Path, message: &str, amend: bool, sign: bool) -> Result<CommitId> {
    if message.len() > MAX_COMMIT_MESSAGE_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "提交消息超过 {} MiB 上限",
            MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
        )));
    }
    if message.contains('\0') {
        return Err(DomainError::InvalidConfig("提交消息包含 NUL 字符".into()));
    }
    let mut args: Vec<&str> = vec!["commit"];
    if amend {
        args.push("--amend");
    }
    if sign {
        args.push("-S");
    }
    // amend + 空 message = 保留原 commit message（--no-edit）；
    // 否则 `-m ""` 会让 git 报 "empty commit message" 拒绝提交
    if amend && message.is_empty() {
        args.push("--no-edit");
        run_git_bytes(repo_path, &args)?;
    } else {
        // stdin 避免消息受系统命令行长度限制，也不会把正文暴露在进程参数列表中。
        args.push("-F");
        args.push("-");
        run_git_stdin(repo_path, &args, message)?;
    }
    let id = run_git_text(repo_path, &["rev-parse", "HEAD"])?;
    Ok(CommitId(id.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{MAX_COMMIT_MESSAGE_BYTES, run};

    #[test]
    fn rejects_unsafe_or_oversized_commit_messages() {
        let path = std::path::Path::new(".");
        assert!(run(path, "bad\0message", false, false).is_err());
        let oversized = "x".repeat(MAX_COMMIT_MESSAGE_BYTES + 1);
        assert!(run(path, &oversized, false, false).is_err());
    }
}
