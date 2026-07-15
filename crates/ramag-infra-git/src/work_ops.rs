//! 工作区写操作：stage / unstage / discard / checkout / branch / list_files

use std::path::Path;

use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, run_git_probe, validate_name_arg,
    validate_positional_arg,
};

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
    parse_file_list(&bytes)
}

fn parse_file_list(bytes: &[u8]) -> Result<Vec<String>> {
    // NUL 切分；末尾常多一个 NUL，过滤空串。非 UTF-8 路径不可转成 String 后安全回传 Git。
    let mut files = Vec::new();
    for (index, path) in bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .enumerate()
    {
        ensure_git_list_room(files.len(), "Git 文件列表")?;
        ensure_git_record_size(path, "Git 文件路径", index + 1)?;
        let path = std::str::from_utf8(path).map_err(|error| {
            DomainError::QueryFailed(format!(
                "解析 Git 文件列表第 {} 条路径失败：路径非 UTF-8：{error}",
                index + 1
            ))
        })?;
        files.push(path.to_string());
    }
    Ok(files)
}

/// 不改工作区
pub fn unstage(repo_path: &Path, paths: &[String]) -> Result<()> {
    // 首次 commit 前没有 HEAD，`git reset HEAD` 会报 ambiguous argument。
    // 此时从 index 移除即可，`--cached` 保留工作区文件不丢内容。
    let has_head = run_git_probe(repo_path, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
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
    validate_positional_arg(target, "checkout 目标")?;
    run_git_bytes(repo_path, &["checkout", target]).map(|_| ())
}

pub fn create_branch(repo_path: &Path, name: &str, base: Option<&str>) -> Result<()> {
    validate_name_arg(name, "分支名")?;
    if let Some(base) = base {
        validate_positional_arg(base, "分支基点")?;
    }
    let is_remote_base = if let Some(base) = base {
        let remote_ref = format!("refs/remotes/{base}");
        run_git_probe(repo_path, &["show-ref", "--verify", "--quiet", &remote_ref])?
    } else {
        false
    };
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
    validate_name_arg(name, "分支名")?;
    let flag = if force { "-D" } else { "-d" };
    run_git_bytes(repo_path, &["branch", flag, name]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::parse_file_list;

    #[test]
    fn file_list_preserves_special_characters() -> ramag_domain::error::Result<()> {
        let files = parse_file_list(b"src/a\tb\nc.rs\0README.md\0")?;
        assert_eq!(files, ["src/a\tb\nc.rs", "README.md"]);
        Ok(())
    }

    #[test]
    fn non_utf8_file_path_is_reported() {
        assert!(parse_file_list(b"ok.rs\0\xff\0").is_err());
    }
}
