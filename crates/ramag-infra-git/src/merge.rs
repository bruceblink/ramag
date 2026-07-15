//! `git merge`。冲突时进入 RepoOperation::Merge，UI 推进 continue / abort

use std::path::Path;

use ramag_domain::error::Result;

use crate::git_cmd::{run_git_bytes, validate_name_arg};

/// no_ff=强制 merge commit；ff_only=必须 ff 否则失败
pub fn start(
    repo_path: &Path,
    branch: &str,
    no_ff: bool,
    ff_only: bool,
    message: Option<&str>,
) -> Result<()> {
    validate_name_arg(branch, "合并分支名")?;
    let mut args: Vec<&str> = vec!["merge"];
    if no_ff {
        args.push("--no-ff");
    }
    if ff_only {
        args.push("--ff-only");
    }
    if let Some(m) = message {
        args.push("-m");
        args.push(m);
    }
    args.push(branch);
    run_git_bytes(repo_path, &args).map(|_| ())
}

pub fn cont(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["merge", "--continue"]).map(|_| ())
}

pub fn abort(repo_path: &Path) -> Result<()> {
    run_git_bytes(repo_path, &["merge", "--abort"]).map(|_| ())
}
