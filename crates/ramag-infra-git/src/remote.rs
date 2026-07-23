//! Remote 管理。`git remote -v` 解析时把同 remote 的 fetch / push URL 合并

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use ramag_domain::entities::Remote;
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, run_git_streaming, run_git_text,
    validate_name_arg, validate_positional_arg,
};

pub fn list(repo_path: &Path) -> Result<Vec<Remote>> {
    let raw = run_git_text(repo_path, &["remote", "-v"])?;
    parse_remotes(&raw)
}

pub fn add(repo_path: &Path, name: &str, url: &str) -> Result<()> {
    validate_name_arg(name, "远程名")?;
    validate_positional_arg(url, "远程 URL")?;
    run_git_bytes(repo_path, &["remote", "add", name, url]).map(|_| ())
}

pub fn remove(repo_path: &Path, name: &str) -> Result<()> {
    validate_name_arg(name, "远程名")?;
    run_git_bytes(repo_path, &["remote", "remove", name]).map(|_| ())
}

pub fn set_url(repo_path: &Path, name: &str, url: &str) -> Result<()> {
    validate_name_arg(name, "远程名")?;
    validate_positional_arg(url, "远程 URL")?;
    run_git_bytes(repo_path, &["remote", "set-url", name, url]).map(|_| ())
}

pub fn rename(repo_path: &Path, old: &str, new: &str) -> Result<()> {
    validate_name_arg(old, "原远程名")?;
    validate_name_arg(new, "新远程名")?;
    run_git_bytes(repo_path, &["remote", "rename", old, new]).map(|_| ())
}

/// remote 为空时拉所有 remote
pub fn fetch(repo_path: &Path, remote: &str) -> Result<()> {
    if remote.is_empty() {
        run_git_bytes(repo_path, &["fetch", "--all", "--prune"]).map(|_| ())
    } else {
        validate_name_arg(remote, "远程名")?;
        run_git_bytes(repo_path, &["fetch", "--prune", remote]).map(|_| ())
    }
}

/// set_upstream=`-u`；force_with_lease 仅在远程状态与本地预期一致才覆盖
pub fn push(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    set_upstream: bool,
    force_with_lease: bool,
) -> Result<()> {
    validate_name_arg(remote, "远程名")?;
    validate_name_arg(branch, "分支名")?;
    let mut args: Vec<&str> = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    if force_with_lease {
        args.push("--force-with-lease");
    }
    args.push(remote);
    args.push(branch);
    run_git_bytes(repo_path, &args).map(|_| ())
}

pub fn pull(repo_path: &Path, remote: &str, branch: &str, rebase: bool) -> Result<()> {
    validate_name_arg(remote, "远程名")?;
    validate_name_arg(branch, "分支名")?;
    let mut args: Vec<&str> = vec!["pull"];
    if rebase {
        args.push("--rebase");
    }
    args.push(remote);
    args.push(branch);
    run_git_bytes(repo_path, &args).map(|_| ())
}

// 支持进度与取消的流式操作。

/// Fetch（带进度 + 可取消）。remote 为空拉全部
pub fn fetch_streaming(
    repo_path: &Path,
    remote: &str,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
) -> Result<()> {
    let mut args: Vec<&str> = vec!["fetch", "--progress", "--prune"];
    if remote.is_empty() {
        args.push("--all");
    } else {
        validate_name_arg(remote, "远程名")?;
        args.push(remote);
    }
    run_git_streaming(repo_path, &args, cancel, progress)
}

/// Push（带进度 + 可取消）
#[allow(clippy::too_many_arguments)]
pub fn push_streaming(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    set_upstream: bool,
    force_with_lease: bool,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
) -> Result<()> {
    validate_name_arg(remote, "远程名")?;
    validate_name_arg(branch, "分支名")?;
    let mut args: Vec<&str> = vec!["push", "--progress"];
    if set_upstream {
        args.push("-u");
    }
    if force_with_lease {
        args.push("--force-with-lease");
    }
    args.push(remote);
    args.push(branch);
    run_git_streaming(repo_path, &args, cancel, progress)
}

/// Pull（带进度 + 可取消）
pub fn pull_streaming(
    repo_path: &Path,
    remote: &str,
    branch: &str,
    rebase: bool,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
) -> Result<()> {
    validate_name_arg(remote, "远程名")?;
    validate_name_arg(branch, "分支名")?;
    let mut args: Vec<&str> = vec!["pull", "--progress"];
    if rebase {
        args.push("--rebase");
    }
    args.push(remote);
    args.push(branch);
    run_git_streaming(repo_path, &args, cancel, progress)
}

/// 一条 remote 两行（fetch 和 push）；fetch==push 时只留 fetch_url
fn parse_remotes(text: &str) -> Result<Vec<Remote>> {
    let mut map: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        ensure_git_record_size(trimmed.as_bytes(), "Git remote 记录", line_index + 1)?;
        // 行格式：name\turl (fetch|push)。
        let (name, rest) = trimmed
            .split_once('\t')
            .ok_or_else(|| remote_parse_error(line_index, "缺少名称与 URL 分隔符"))?;
        let (url, kind) = rest
            .rsplit_once(' ')
            .ok_or_else(|| remote_parse_error(line_index, "缺少 fetch/push 类型"))?;
        let kind = kind
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(|| remote_parse_error(line_index, "fetch/push 类型格式无效"))?;
        let url = url.trim();
        if name.is_empty() || url.is_empty() {
            return Err(remote_parse_error(line_index, "remote 名称或 URL 为空"));
        }
        if !map.contains_key(name) {
            ensure_git_list_room(map.len(), "Git remote 列表")?;
        }
        let entry = map.entry(name.to_string()).or_insert((None, None));
        match kind {
            "fetch" if entry.0.is_none() => entry.0 = Some(url.to_string()),
            "push" if entry.1.is_none() => entry.1 = Some(url.to_string()),
            "fetch" | "push" => {
                return Err(remote_parse_error(
                    line_index,
                    "同一 remote 存在多个同类型 URL，当前界面无法安全表示",
                ));
            }
            _ => {
                return Err(remote_parse_error(line_index, "未知 remote URL 类型"));
            }
        }
    }
    map.into_iter()
        .map(|(name, (fetch, push))| {
            let fetch_url = fetch.ok_or_else(|| {
                DomainError::QueryFailed(format!("解析 Git remote {name} 失败：缺少 fetch URL"))
            })?;
            let push_url = push.filter(|p| p != &fetch_url);
            Ok(Remote {
                name,
                fetch_url,
                push_url,
            })
        })
        .collect()
}

fn remote_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git remote 第 {} 条记录失败：{reason}",
        index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_remote_with_same_fetch_push() -> Result<()> {
        let text = "\
origin\thttps://example.com/r.git (fetch)
origin\thttps://example.com/r.git (push)
";
        let r = parse_remotes(text)?;
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "origin");
        assert_eq!(r[0].fetch_url, "https://example.com/r.git");
        assert!(r[0].push_url.is_none());
        Ok(())
    }

    #[test]
    fn parses_distinct_push_url() -> Result<()> {
        let text = "\
origin\thttps://example.com/r.git (fetch)
origin\tgit@example.com:r.git (push)
";
        let r = parse_remotes(text)?;
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].fetch_url, "https://example.com/r.git");
        assert_eq!(r[0].push_url.as_deref(), Some("git@example.com:r.git"));
        Ok(())
    }

    #[test]
    fn parses_multiple_remotes_sorted_by_name() -> Result<()> {
        let text = "\
upstream\thttps://up.com/r.git (fetch)
upstream\thttps://up.com/r.git (push)
origin\thttps://o.com/r.git (fetch)
origin\thttps://o.com/r.git (push)
";
        let r = parse_remotes(text)?;
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "origin");
        assert_eq!(r[1].name, "upstream");
        Ok(())
    }

    #[test]
    fn malformed_or_duplicate_remote_is_reported() {
        assert!(parse_remotes("origin https://example.com (fetch)\n").is_err());
        assert!(parse_remotes("origin\thttps://example.com (other)\n").is_err());
        assert!(
            parse_remotes(
                "origin\thttps://one.example (fetch)\norigin\thttps://two.example (fetch)\n"
            )
            .is_err()
        );
    }
}
