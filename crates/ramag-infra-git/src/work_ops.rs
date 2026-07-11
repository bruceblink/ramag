//! 工作区写操作：stage / unstage / discard / checkout / branch / list_files

use std::path::Path;

use ramag_domain::error::Result;

use crate::git_cmd::run_git_bytes;

pub fn stage(repo_path: &Path, paths: &[String]) -> Result<()> {
    let mut args: Vec<&str> = vec!["add", "--"];
    for p in paths {
        args.push(p);
    }
    run_git_bytes(repo_path, &args).map(|_| ())
}

/// `git ls-files --cached --others --exclude-standard -z`
pub fn list_files(repo_path: &Path) -> Result<Vec<String>> {
    let bytes = run_git_bytes(
        repo_path,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    // NUL 切分；末尾常多一个 NUL，过滤空串
    let paths: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Ok(paths)
}

/// 不改工作区
pub fn unstage(repo_path: &Path, paths: &[String]) -> Result<()> {
    // 首次 commit 前没有 HEAD，`git reset HEAD` 会报 ambiguous argument。
    // 此时从 index 移除即可，`--cached` 保留工作区文件不丢内容。
    let has_head = run_git_bytes(repo_path, &["rev-parse", "--verify", "HEAD"]).is_ok();
    let mut args: Vec<&str> = if has_head {
        vec!["reset", "HEAD", "--"]
    } else {
        vec!["rm", "--cached", "--"]
    };
    for p in paths {
        args.push(p);
    }
    run_git_bytes(repo_path, &args).map(|_| ())
}

/// 工作区还原到暂存区版本（仅 tracked 文件）
pub fn discard(repo_path: &Path, paths: &[String]) -> Result<()> {
    let mut args: Vec<&str> = vec!["checkout", "--"];
    for p in paths {
        args.push(p);
    }
    run_git_bytes(repo_path, &args).map(|_| ())
}

pub fn checkout(repo_path: &Path, target: &str) -> Result<()> {
    run_git_bytes(repo_path, &["checkout", target]).map(|_| ())
}

pub fn create_branch(repo_path: &Path, name: &str, base: Option<&str>) -> Result<()> {
    let is_remote_base = base.is_some_and(|base| {
        let remote_ref = format!("refs/remotes/{base}");
        run_git_bytes(repo_path, &["show-ref", "--verify", "--quiet", &remote_ref]).is_ok()
    });
    let mut args: Vec<&str> = if is_remote_base {
        // 不依赖用户的 branch.autoSetupMerge 配置，兑现 UI“创建本地副本”的 tracking 承诺。
        vec!["branch", "--track", name]
    } else {
        vec!["branch", name]
    };
    if let Some(b) = base {
        args.push(b);
    }
    run_git_bytes(repo_path, &args).map(|_| ())
}

pub fn delete_branch(repo_path: &Path, name: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    run_git_bytes(repo_path, &["branch", flag, name]).map(|_| ())
}
