//! Remote 管理。`git remote -v` 解析时把同 remote 的 fetch / push URL 合并

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use ramag_domain::entities::Remote;
use ramag_domain::error::Result;

use crate::git_cmd::{run_git_bytes, run_git_streaming, run_git_text};

pub fn list(repo_path: &Path) -> Result<Vec<Remote>> {
    let raw = run_git_text(repo_path, &["remote", "-v"])?;
    Ok(parse_remotes(&raw))
}

pub fn add(repo_path: &Path, name: &str, url: &str) -> Result<()> {
    run_git_bytes(repo_path, &["remote", "add", name, url]).map(|_| ())
}

pub fn remove(repo_path: &Path, name: &str) -> Result<()> {
    run_git_bytes(repo_path, &["remote", "remove", name]).map(|_| ())
}

pub fn set_url(repo_path: &Path, name: &str, url: &str) -> Result<()> {
    run_git_bytes(repo_path, &["remote", "set-url", name, url]).map(|_| ())
}

pub fn rename(repo_path: &Path, old: &str, new: &str) -> Result<()> {
    run_git_bytes(repo_path, &["remote", "rename", old, new]).map(|_| ())
}

/// remote 为空时拉所有 remote
pub fn fetch(repo_path: &Path, remote: &str) -> Result<()> {
    if remote.is_empty() {
        run_git_bytes(repo_path, &["fetch", "--all", "--prune"]).map(|_| ())
    } else {
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
    let mut args: Vec<&str> = vec!["pull"];
    if rebase {
        args.push("--rebase");
    }
    args.push(remote);
    args.push(branch);
    run_git_bytes(repo_path, &args).map(|_| ())
}

// —— 流式变体（带进度 + 可取消）：与 clone_repo_streaming 同哲学，供 UI 展示进度并中止 ——

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
    let mut args: Vec<&str> = vec!["pull", "--progress"];
    if rebase {
        args.push("--rebase");
    }
    args.push(remote);
    args.push(branch);
    run_git_streaming(repo_path, &args, cancel, progress)
}

/// 一条 remote 两行（fetch 和 push）；fetch==push 时只留 fetch_url
fn parse_remotes(text: &str) -> Vec<Remote> {
    let mut map: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // name\turl (fetch|push)
        let mut parts = trimmed.splitn(2, '\t');
        let name = match parts.next() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let rest = match parts.next() {
            Some(r) => r,
            None => continue,
        };
        let (url, kind) = match rest.rsplit_once(' ') {
            Some((u, k)) => (
                u.trim().to_string(),
                k.trim_matches(|c| c == '(' || c == ')'),
            ),
            None => (rest.trim().to_string(), ""),
        };
        let entry = map.entry(name).or_insert((None, None));
        match kind {
            "fetch" => entry.0 = Some(url),
            "push" => entry.1 = Some(url),
            _ => {
                if entry.0.is_none() {
                    entry.0 = Some(url);
                }
            }
        }
    }
    map.into_iter()
        .filter_map(|(name, (fetch, push))| {
            let fetch_url = fetch?;
            let push_url = push.filter(|p| p != &fetch_url);
            Some(Remote {
                name,
                fetch_url,
                push_url,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_remote_with_same_fetch_push() {
        let text = "\
origin\thttps://example.com/r.git (fetch)
origin\thttps://example.com/r.git (push)
";
        let r = parse_remotes(text);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "origin");
        assert_eq!(r[0].fetch_url, "https://example.com/r.git");
        assert!(r[0].push_url.is_none());
    }

    #[test]
    fn parses_distinct_push_url() {
        let text = "\
origin\thttps://example.com/r.git (fetch)
origin\tgit@example.com:r.git (push)
";
        let r = parse_remotes(text);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].fetch_url, "https://example.com/r.git");
        assert_eq!(r[0].push_url.as_deref(), Some("git@example.com:r.git"));
    }

    #[test]
    fn parses_multiple_remotes_sorted_by_name() {
        let text = "\
upstream\thttps://up.com/r.git (fetch)
upstream\thttps://up.com/r.git (push)
origin\thttps://o.com/r.git (fetch)
origin\thttps://o.com/r.git (push)
";
        let r = parse_remotes(text);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "origin");
        assert_eq!(r[1].name, "upstream");
    }
}
