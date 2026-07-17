//! `git merge`。冲突时进入 RepoOperation::Merge，UI 推进 continue / abort

use std::path::Path;

use ramag_domain::entities::MAX_COMMIT_MESSAGE_BYTES;
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{run_git_bytes, validate_name_arg};
use crate::temp_file::TempFile;

/// no_ff=强制 merge commit；ff_only=必须 ff 否则失败
pub fn start(
    repo_path: &Path,
    branch: &str,
    no_ff: bool,
    ff_only: bool,
    message: Option<&str>,
) -> Result<()> {
    validate_name_arg(branch, "合并分支名")?;
    if let Some(message) = message {
        validate_message(message)?;
    }
    let mut args: Vec<&str> = vec!["merge"];
    if no_ff {
        args.push("--no-ff");
    }
    if ff_only {
        args.push("--ff-only");
    }
    if let Some(m) = message {
        let message_file = TempFile::create("ramag_merge", "txt", m.as_bytes())?;
        let message_path = message_file
            .path()
            .to_str()
            .ok_or_else(|| DomainError::Other("合并消息临时路径含非 UTF-8 字符".into()))?;
        args.push("-F");
        args.push(message_path);
        args.push(branch);
        return run_git_bytes(repo_path, &args).map(|_| ());
    }
    args.push(branch);
    run_git_bytes(repo_path, &args).map(|_| ())
}

pub(crate) fn validate_message(message: &str) -> Result<()> {
    if message.len() > MAX_COMMIT_MESSAGE_BYTES || message.contains('\0') {
        return Err(DomainError::InvalidConfig(format!(
            "合并消息超过 {} MiB 上限或包含 NUL 字符",
            MAX_COMMIT_MESSAGE_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

pub fn cont(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["merge", "--continue"]).map(|_| ())
}

pub fn abort(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["merge", "--abort"]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_message_has_stdin_boundary() {
        assert!(validate_message("merge feature").is_ok());
        assert!(validate_message("bad\0message").is_err());
        assert!(validate_message(&"m".repeat(MAX_COMMIT_MESSAGE_BYTES + 1)).is_err());
    }
}
