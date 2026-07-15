//! 工作区状态 + 分支。HEAD 走 gix；文件变更与 ahead/behind 共用
//! `git status --porcelain=v2 --branch -z`；进行中操作看 .git/ 标记文件。

use std::path::Path;

use ramag_domain::entities::{
    Branch, BranchKind, CommitId, FileChangeKind, FileStatus, RepoOperation, WorkingTreeStatus,
};
use ramag_domain::error::{DomainError, Result};

use crate::errors::{map_branch_error, map_status_error};
use crate::git_cmd::{ensure_git_list_room, ensure_git_record_size, run_git_bytes, run_git_text};

pub fn collect_status(repo: &gix::Repository, repo_path: &Path) -> Result<WorkingTreeStatus> {
    let head = repo.head().map_err(map_status_error)?;
    let head_branch = head
        .referent_name()
        .map(|name| short_branch_name(name.as_bstr()));
    // unborn HEAD 是新仓库的正常状态；对象读取或 peel 失败则必须上报。
    let head_commit = head
        .try_into_peeled_id()
        .map_err(map_status_error)?
        .map(|id| CommitId(id.to_string()).short().to_string());

    let operation = detect_operation(repo);
    let (files, ahead, behind) = parse_porcelain_v2(repo_path)?;

    Ok(WorkingTreeStatus {
        head_branch,
        head_commit,
        operation,
        files,
        ahead,
        behind,
    })
}

/// 本地分支同时填 upstream / ahead / behind
pub fn list_branches(repo: &gix::Repository, kind: BranchKind) -> Result<Vec<Branch>> {
    // 必须按 symbolic ref 名匹配 is_head，commit_id 比较会让指向同一 commit 的多分支误标
    let head_branch_name = match repo.head_name() {
        Ok(Some(name)) => Some(short_branch_name(name.as_bstr())),
        Ok(None) => None,
        Err(error) => return Err(map_branch_error(error)),
    };
    let platform = repo.references().map_err(map_branch_error)?;

    let iter = match kind {
        BranchKind::Local => platform.local_branches(),
        BranchKind::Remote => platform.remote_branches(),
    }
    .map_err(map_branch_error)?;

    // 远程分支本身就是上游，无 upstream tracking
    let tracking = if matches!(kind, BranchKind::Local) {
        fetch_branch_tracking(repo)?
    } else {
        std::collections::HashMap::new()
    };

    let mut branches = Vec::new();
    for r in iter {
        let r = match r {
            Ok(r) => r,
            Err(error) => {
                tracing::warn!(error = %error, "vcs: skip unreadable branch reference");
                continue;
            }
        };
        let full = r.name().as_bstr();
        let short = short_branch_name(full);
        let commit_id = match r.target().try_id() {
            Some(id) => CommitId(id.to_string()),
            None => continue,
        };
        // 远程分支永远不是 HEAD
        let is_head = matches!(kind, BranchKind::Local)
            && head_branch_name.as_deref() == Some(short.as_str());

        let (upstream, ahead, behind) = if let Some(t) = tracking.get(&short) {
            (Some(t.upstream.clone()), t.ahead, t.behind)
        } else {
            (None, None, None)
        };

        ensure_git_list_room(branches.len(), "Git 分支列表")?;
        branches.push(Branch {
            name: short,
            kind,
            commit: commit_id,
            is_head,
            upstream,
            ahead,
            behind,
        });
    }
    Ok(branches)
}

struct TrackInfo {
    upstream: String,
    ahead: Option<usize>,
    behind: Option<usize>,
}

/// `git for-each-ref` 批量取本地分支的 upstream + track 计数
fn fetch_branch_tracking(
    repo: &gix::Repository,
) -> Result<std::collections::HashMap<String, TrackInfo>> {
    let repo_path = repo.git_dir().parent().unwrap_or(repo.git_dir());
    let out = run_git_text(
        repo_path,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)\t%(upstream:track)",
            "refs/heads/",
        ],
    )?;

    let mut map = std::collections::HashMap::new();
    for (line_index, line) in out.lines().enumerate() {
        ensure_git_record_size(line.as_bytes(), "Git 分支跟踪记录", line_index + 1)?;
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 || parts[0].is_empty() {
            return Err(branch_tracking_parse_error(
                line_index,
                "字段数量异常或分支名为空",
            ));
        }
        if parts[1].is_empty() {
            continue;
        }
        let branch = parts[0].to_string();
        let upstream = parts[1].to_string();
        let (ahead, behind) = parse_track(parts[2], line_index)?;
        if !map.contains_key(&branch) {
            ensure_git_list_room(map.len(), "Git 分支跟踪列表")?;
        }
        map.insert(
            branch,
            TrackInfo {
                upstream,
                ahead,
                behind,
            },
        );
    }
    Ok(map)
}

/// `%(upstream:track)` 形如 `[ahead 2, behind 1]`
fn parse_track(s: &str, line_index: usize) -> Result<(Option<usize>, Option<usize>)> {
    let s = s.trim();
    if s.is_empty() || s == "[gone]" {
        return Ok((None, None));
    }
    let s = s
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| branch_tracking_parse_error(line_index, "track 状态缺少方括号"))?;
    let mut ahead = None;
    let mut behind = None;
    for part in s.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            if ahead.is_some() {
                return Err(branch_tracking_parse_error(line_index, "ahead 计数重复"));
            }
            ahead = Some(n.trim().parse().map_err(|error| {
                branch_tracking_parse_error(line_index, &format!("ahead 计数无效：{error}"))
            })?);
        } else if let Some(n) = part.strip_prefix("behind ") {
            if behind.is_some() {
                return Err(branch_tracking_parse_error(line_index, "behind 计数重复"));
            }
            behind = Some(n.trim().parse().map_err(|error| {
                branch_tracking_parse_error(line_index, &format!("behind 计数无效：{error}"))
            })?);
        } else {
            return Err(branch_tracking_parse_error(line_index, "未知 track 状态"));
        }
    }
    Ok((ahead, behind))
}

fn branch_tracking_parse_error(line_index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git 分支跟踪信息第 {} 条记录失败：{reason}",
        line_index + 1
    ))
}

/// `git status --porcelain=v2 --branch -z`：NUL 分记录，首字节标 entry type；
/// `# branch.ab +N -N` 同一回包提供 ahead/behind，避免额外启动 `git rev-list`。
fn parse_porcelain_v2(repo_path: &Path) -> Result<(Vec<FileStatus>, Option<usize>, Option<usize>)> {
    let bytes = run_git_bytes(repo_path, &["status", "--porcelain=v2", "--branch", "-z"])?;
    parse_porcelain_bytes(&bytes)
}

fn parse_porcelain_bytes(bytes: &[u8]) -> Result<(Vec<FileStatus>, Option<usize>, Option<usize>)> {
    let mut out = Vec::new();
    let mut ahead_behind = None;
    let mut iter = bytes.split(|&b| b == 0).filter(|s| !s.is_empty());
    let mut record_index = 0;
    while let Some(record) = iter.next() {
        record_index += 1;
        ensure_git_record_size(record, "Git 工作区状态记录", record_index)?;
        let first = record
            .first()
            .copied()
            .ok_or_else(|| status_parse_error(record_index, "记录为空"))?;
        match first {
            b'#' => parse_branch_header(record, record_index, &mut ahead_behind)?,
            b'1' => {
                ensure_git_list_room(out.len(), "Git 工作区文件状态")?;
                out.push(parse_ordinary(record, record_index)?);
            }
            b'2' => {
                // type 2 紧跟一条 NUL 分隔的 old_path
                let old_path = iter
                    .next()
                    .ok_or_else(|| status_parse_error(record_index, "rename 记录缺少旧路径"))?;
                ensure_git_record_size(old_path, "Git 工作区旧路径", record_index)?;
                ensure_git_list_room(out.len(), "Git 工作区文件状态")?;
                out.push(parse_rename(record, old_path, record_index)?);
            }
            b'?' => {
                ensure_git_list_room(out.len(), "Git 工作区文件状态")?;
                out.push(parse_untracked(record, record_index)?);
            }
            b'u' => {
                ensure_git_list_room(out.len(), "Git 工作区文件状态")?;
                out.push(parse_unmerged(record, record_index)?);
            }
            other => {
                return Err(status_parse_error(
                    record_index,
                    &format!("未知记录类型 0x{other:02x}"),
                ));
            }
        }
    }
    let (ahead, behind) = ahead_behind
        .map(|(ahead, behind)| (Some(ahead), Some(behind)))
        .unwrap_or((None, None));
    Ok((out, ahead, behind))
}

fn parse_branch_header(
    record: &[u8],
    index: usize,
    ahead_behind: &mut Option<(usize, usize)>,
) -> Result<()> {
    let text = decode_status_record(record, index)?;
    let Some(value) = text.strip_prefix("# branch.ab ") else {
        // branch.oid/head/upstream 是已知头；Git 允许以后增加头字段，调用方须可忽略。
        if text.starts_with("# ") {
            return Ok(());
        }
        return Err(status_parse_error(index, "分支头记录格式异常"));
    };
    if ahead_behind.is_some() {
        return Err(status_parse_error(index, "branch.ab 重复"));
    }
    let mut parts = value.split_ascii_whitespace();
    let ahead = parse_branch_count(parts.next(), '+', index)?;
    let behind = parse_branch_count(parts.next(), '-', index)?;
    if parts.next().is_some() {
        return Err(status_parse_error(index, "branch.ab 字段数量异常"));
    }
    *ahead_behind = Some((ahead, behind));
    Ok(())
}

fn parse_branch_count(value: Option<&str>, prefix: char, index: usize) -> Result<usize> {
    value
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| status_parse_error(index, "branch.ab 计数前缀异常"))?
        .parse()
        .map_err(|error| status_parse_error(index, &format!("branch.ab 计数无效：{error}")))
}

fn parse_ordinary(record: &[u8], index: usize) -> Result<FileStatus> {
    // "1 XY sub mH mI mW hH hI path"
    let text = decode_status_record(record, index)?;
    let parts: Vec<&str> = text.splitn(9, ' ').collect();
    if parts.len() != 9 || parts[0] != "1" {
        return Err(status_parse_error(index, "普通记录字段数量异常"));
    }
    let path = parts[8];
    if path.is_empty() {
        return Err(status_parse_error(index, "普通记录路径为空"));
    }
    let (staged, unstaged) = parse_xy(parts[1], index)?;
    Ok(FileStatus {
        path: path.to_string(),
        old_path: None,
        staged,
        unstaged,
    })
}

fn parse_rename(record: &[u8], old_path: &[u8], index: usize) -> Result<FileStatus> {
    // "2 XY sub mH mI mW hH hI Xscore newpath"
    let text = decode_status_record(record, index)?;
    let parts: Vec<&str> = text.splitn(10, ' ').collect();
    if parts.len() != 10 || parts[0] != "2" {
        return Err(status_parse_error(index, "rename 记录字段数量异常"));
    }
    if parts[9].is_empty() || old_path.is_empty() {
        return Err(status_parse_error(index, "rename 路径为空"));
    }
    let (staged, unstaged) = parse_xy(parts[1], index)?;
    let old_path = std::str::from_utf8(old_path)
        .map_err(|error| status_parse_error(index, &format!("旧路径非 UTF-8：{error}")))?;
    Ok(FileStatus {
        path: parts[9].to_string(),
        old_path: Some(old_path.to_string()),
        staged,
        unstaged,
    })
}

fn parse_untracked(record: &[u8], index: usize) -> Result<FileStatus> {
    // "? path"
    let text = decode_status_record(record, index)?;
    let path = text
        .strip_prefix("? ")
        .ok_or_else(|| status_parse_error(index, "未跟踪记录前缀异常"))?;
    if path.is_empty() {
        return Err(status_parse_error(index, "未跟踪路径为空"));
    }
    Ok(FileStatus {
        path: path.to_string(),
        old_path: None,
        staged: None,
        unstaged: Some(FileChangeKind::Untracked),
    })
}

fn parse_unmerged(record: &[u8], index: usize) -> Result<FileStatus> {
    // "u XY sub m1 m2 m3 mW h1 h2 h3 path"
    let text = decode_status_record(record, index)?;
    let parts: Vec<&str> = text.splitn(11, ' ').collect();
    if parts.len() != 11 || parts[0] != "u" {
        return Err(status_parse_error(index, "冲突记录字段数量异常"));
    }
    if parts[10].is_empty() {
        return Err(status_parse_error(index, "冲突路径为空"));
    }
    parse_xy(parts[1], index)?;
    Ok(FileStatus {
        path: parts[10].to_string(),
        old_path: None,
        staged: Some(FileChangeKind::Conflicted),
        unstaged: Some(FileChangeKind::Conflicted),
    })
}

fn parse_xy(xy: &str, index: usize) -> Result<(Option<FileChangeKind>, Option<FileChangeKind>)> {
    if xy.len() != 2 || !xy.is_ascii() {
        return Err(status_parse_error(index, "XY 状态码格式异常"));
    }
    let mut chars = xy.chars();
    let x = chars
        .next()
        .ok_or_else(|| status_parse_error(index, "缺少暂存区状态码"))?;
    let y = chars
        .next()
        .ok_or_else(|| status_parse_error(index, "缺少工作区状态码"))?;
    Ok((code_to_kind(x, index)?, code_to_kind(y, index)?))
}

fn code_to_kind(c: char, index: usize) -> Result<Option<FileChangeKind>> {
    let kind = match c {
        ' ' | '.' => None,
        'M' => Some(FileChangeKind::Modified),
        'A' => Some(FileChangeKind::Added),
        'D' => Some(FileChangeKind::Deleted),
        'R' => Some(FileChangeKind::Renamed),
        'C' => Some(FileChangeKind::Copied),
        'T' => Some(FileChangeKind::TypeChanged),
        'U' => Some(FileChangeKind::Conflicted),
        '?' => Some(FileChangeKind::Untracked),
        other => {
            return Err(status_parse_error(index, &format!("未知状态码 {other:?}")));
        }
    };
    Ok(kind)
}

fn decode_status_record(record: &[u8], index: usize) -> Result<&str> {
    std::str::from_utf8(record)
        .map_err(|error| status_parse_error(index, &format!("路径或记录非 UTF-8：{error}")))
}

fn status_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git 工作区状态第 {index} 条记录失败：{reason}"
    ))
}

fn detect_operation(repo: &gix::Repository) -> Option<RepoOperation> {
    let git_dir = repo.git_dir();
    if git_dir.join("MERGE_HEAD").exists() {
        return Some(RepoOperation::Merge);
    }
    if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        return Some(RepoOperation::Rebase);
    }
    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return Some(RepoOperation::CherryPick);
    }
    if git_dir.join("REVERT_HEAD").exists() {
        return Some(RepoOperation::Revert);
    }
    None
}

fn short_branch_name(full: &gix::bstr::BStr) -> String {
    let s = full.to_string();
    s.strip_prefix("refs/heads/")
        .or_else(|| s.strip_prefix("refs/remotes/"))
        .map(|x| x.to_string())
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xy_codes() -> Result<()> {
        assert_eq!(code_to_kind(' ', 1)?, None);
        assert_eq!(code_to_kind('.', 1)?, None);
        assert_eq!(code_to_kind('M', 1)?, Some(FileChangeKind::Modified));
        assert_eq!(code_to_kind('A', 1)?, Some(FileChangeKind::Added));
        assert_eq!(code_to_kind('?', 1)?, Some(FileChangeKind::Untracked));
        assert!(code_to_kind('X', 1).is_err());
        Ok(())
    }

    #[test]
    fn parses_porcelain_v2_records() -> Result<()> {
        let raw = b"1 M. N... 100644 100644 100644 abc def src/lib.rs\0\
                    2 R. N... 100644 100644 100644 abc def R100 new.rs\0old.rs\0\
                    ? new file.rs\0\
                    u UU N... 100644 100644 100644 100644 abc def ghi conflict.rs\0";
        let (files, ahead, behind) = parse_porcelain_bytes(raw)?;
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "new file.rs");
        assert_eq!(files[3].staged, Some(FileChangeKind::Conflicted));
        assert_eq!((ahead, behind), (None, None));
        Ok(())
    }

    #[test]
    fn parses_branch_ahead_behind_without_extra_command() -> Result<()> {
        let raw = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0\
                    # branch.ab +12 -3\0? file.txt\0";
        let (files, ahead, behind) = parse_porcelain_bytes(raw)?;
        assert_eq!(files.len(), 1);
        assert_eq!((ahead, behind), (Some(12), Some(3)));
        Ok(())
    }

    #[test]
    fn malformed_or_non_utf8_status_is_reported() {
        assert!(parse_porcelain_bytes(b"1 M. incomplete\0").is_err());
        assert!(parse_porcelain_bytes(b"2 R. incomplete\0").is_err());
        assert!(parse_porcelain_bytes(b"x unknown\0").is_err());
        assert!(parse_porcelain_bytes(b"? \xff\0").is_err());
        assert!(parse_porcelain_bytes(b"# branch.ab ahead -1\0").is_err());
        assert!(parse_porcelain_bytes(b"# branch.ab +1 -2 extra\0").is_err());
        assert!(parse_porcelain_bytes(b"# branch.ab +1 -2\0# branch.ab +1 -2\0").is_err());
    }

    #[test]
    fn branch_track_parser_preserves_gone_and_rejects_bad_counts() -> Result<()> {
        assert_eq!(parse_track("[ahead 2, behind 1]", 0)?, (Some(2), Some(1)));
        assert_eq!(parse_track("[gone]", 0)?, (None, None));
        assert!(parse_track("[ahead many]", 0).is_err());
        assert!(parse_track("ahead 1", 0).is_err());
        assert!(parse_track("[unknown 1]", 0).is_err());
        Ok(())
    }
}
