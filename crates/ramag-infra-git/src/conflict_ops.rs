//! 冲突解决：`git checkout --ours/--theirs` + `git add` 一步标记已解决

use std::path::Path;

use ramag_domain::error::Result;

use crate::git_cmd::run_git_pathspecs;

/// HEAD 侧
pub fn use_ours(repo_path: &Path, paths: &[String]) -> Result<()> {
    apply_side(repo_path, "--ours", paths)
}

/// 对方分支侧
pub fn use_theirs(repo_path: &Path, paths: &[String]) -> Result<()> {
    apply_side(repo_path, "--theirs", paths)
}

fn apply_side(repo_path: &Path, side: &str, paths: &[String]) -> Result<()> {
    run_git_pathspecs(
        repo_path,
        &[
            "checkout",
            side,
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        paths,
    )?;
    run_git_pathspecs(
        repo_path,
        &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
        paths,
    )
}
