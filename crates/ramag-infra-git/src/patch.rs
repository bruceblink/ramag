//! 行级 / 分块 stage：`git apply --cached`。`--recount` 重算行数、`--unidiff-zero` 容忍零上下文

use std::path::Path;

use ramag_domain::entities::MAX_GIT_PATCH_BYTES;
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::run_git_stdin;

pub fn stage(repo_path: &Path, patch: &str) -> Result<()> {
    validate_patch(patch)?;
    run_git_stdin(
        repo_path,
        &[
            "apply",
            "--cached",
            "--recount",
            "--unidiff-zero",
            "--inaccurate-eof",
            "-",
        ],
        patch,
    )
}

/// reverse 模式撤回，不影响工作区
pub fn unstage(repo_path: &Path, patch: &str) -> Result<()> {
    validate_patch(patch)?;
    run_git_stdin(
        repo_path,
        &[
            "apply",
            "--cached",
            "--reverse",
            "--recount",
            "--unidiff-zero",
            "--inaccurate-eof",
            "-",
        ],
        patch,
    )
}

/// hunk 级回滚到 HEAD（不走暂存区）。失败常见于工作区改动与 patch 上下文不匹配
pub fn discard(repo_path: &Path, patch: &str) -> Result<()> {
    validate_patch(patch)?;
    run_git_stdin(
        repo_path,
        &[
            "apply",
            "--reverse",
            "--recount",
            "--unidiff-zero",
            "--inaccurate-eof",
            "-",
        ],
        patch,
    )
}

pub(crate) fn validate_patch(patch: &str) -> Result<()> {
    if patch.is_empty() {
        return Err(DomainError::InvalidConfig("Git patch 不能为空".into()));
    }
    if patch.len() > MAX_GIT_PATCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Git patch 超过 {} MiB 安全上限",
            MAX_GIT_PATCH_BYTES / 1024 / 1024
        )));
    }
    if patch.contains('\0') {
        return Err(DomainError::InvalidConfig(
            "Git patch 不能包含 NUL 字符".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_input_has_explicit_boundary() {
        assert!(validate_patch("diff --git a/a b/a\n").is_ok());
        assert!(validate_patch("").is_err());
        assert!(validate_patch("bad\0patch").is_err());
        assert!(validate_patch(&"x".repeat(MAX_GIT_PATCH_BYTES + 1)).is_err());
    }
}
