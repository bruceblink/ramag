//! Stash 列表 + 操作。`stash@{N}` 索引由 git 维护，UI 按 idx 反查

use std::path::Path;

use ramag_domain::entities::{CommitId, Stash, StashId};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{ensure_git_list_room, ensure_git_record_size, run_git_bytes, run_git_text};

pub fn save(repo_path: &Path, message: Option<&str>, include_untracked: bool) -> Result<()> {
    let mut args: Vec<&str> = vec!["stash", "push"];
    if include_untracked {
        args.push("-u");
    }
    if let Some(m) = message {
        args.push("-m");
        args.push(m);
    }
    run_git_bytes(repo_path, &args).map(|_| ())
}

/// pop=true 应用后删除
pub fn apply(repo_path: &Path, idx: usize, pop: bool) -> Result<()> {
    let cmd = if pop { "pop" } else { "apply" };
    let r = format!("stash@{{{idx}}}");
    run_git_bytes(repo_path, &["stash", cmd, &r]).map(|_| ())
}

pub fn drop(repo_path: &Path, idx: usize) -> Result<()> {
    let r = format!("stash@{{{idx}}}");
    run_git_bytes(repo_path, &["stash", "drop", &r]).map(|_| ())
}

pub fn list(repo_path: &Path) -> Result<Vec<Stash>> {
    // `|` 分字段：%gd selector / %H commit / %ct ts / %s message
    let out = run_git_text(
        repo_path,
        &["stash", "list", "--pretty=format:%gd|%H|%ct|%s"],
    )?;
    parse_stash_output(&out)
}

fn parse_stash_output(out: &str) -> Result<Vec<Stash>> {
    let mut result = Vec::new();
    for (line_index, line) in out.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        ensure_git_list_room(result.len(), "Git stash 列表")?;
        ensure_git_record_size(line.as_bytes(), "Git stash 记录", line_index + 1)?;
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() != 4 {
            return Err(stash_parse_error(line_index, "字段数量不足"));
        }
        let idx_str = parts[0]
            .strip_prefix("stash@{")
            .and_then(|value| value.strip_suffix('}'))
            .ok_or_else(|| stash_parse_error(line_index, "selector 格式无效"))?;
        let idx = idx_str
            .parse::<usize>()
            .map_err(|error| stash_parse_error(line_index, &format!("索引无效：{error}")))?;
        if parts[1].is_empty() {
            return Err(stash_parse_error(line_index, "commit id 为空"));
        }
        let commit = CommitId(parts[1].to_string());
        let ts = parts[2]
            .parse::<i64>()
            .map_err(|error| stash_parse_error(line_index, &format!("时间非整数：{error}")))?;
        let timestamp = chrono::DateTime::from_timestamp(ts, 0)
            .ok_or_else(|| stash_parse_error(line_index, "时间超出支持范围"))?;
        let message = parts[3].to_string();
        result.push(Stash {
            id: StashId(idx),
            message,
            commit,
            timestamp,
        });
    }
    Ok(result)
}

fn stash_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git stash 第 {} 条记录失败：{reason}",
        index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stash_output() -> Result<()> {
        let stashes = parse_stash_output("stash@{0}|abc123|1700000000|WIP on main: work")?;
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].id.0, 0);
        assert_eq!(stashes[0].commit.0, "abc123");
        assert_eq!(stashes[0].message, "WIP on main: work");
        Ok(())
    }

    #[test]
    fn malformed_stash_is_reported() {
        assert!(parse_stash_output("stash@{x}|abc123|1700000000|bad").is_err());
        assert!(parse_stash_output("stash@{0}|abc123|bad-time|bad").is_err());
        assert!(parse_stash_output("incomplete").is_err());
    }
}
