//! Reset / Revert 操作

use std::path::Path;

use ramag_domain::entities::ResetKind;
use ramag_domain::error::Result;

use crate::git_cmd::run_git_bytes;

pub fn reset(repo_path: &Path, target: &str, kind: ResetKind) -> Result<()> {
    let flag = match kind {
        ResetKind::Soft => "--soft",
        ResetKind::Mixed => "--mixed",
        ResetKind::Hard => "--hard",
    };
    run_git_bytes(repo_path, &["reset", flag, target]).map(|_| ())
}

/// 生成反向 commit 撤销指定 commit
pub fn revert(repo_path: &Path, commit: &str) -> Result<()> {
    // --no-edit 避免弹编辑器
    run_git_bytes(repo_path, &["revert", "--no-edit", commit]).map(|_| ())
}

/// revert 冲突解决后继续
pub fn revert_continue(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["revert", "--continue"]).map(|_| ())
}

/// revert 冲突后中止，回滚到 revert 前状态
pub fn revert_abort(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["revert", "--abort"]).map(|_| ())
}
