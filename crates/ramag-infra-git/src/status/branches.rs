//! Git 分支查询与记录解析。

use std::path::Path;

use ramag_domain::entities::{Branch, BranchKind, CommitId};
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::git_cmd::{ensure_git_list_room, ensure_git_record_size, run_git_bytes};

const BRANCH_FORMAT: &str =
    "%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track)%00%(symref)%1e";

/// 查询指定类型的分支。
pub fn list_branches(repo_path: &Path, kind: BranchKind) -> Result<Vec<Branch>> {
    let refs = match kind {
        BranchKind::Local => &["refs/heads/"][..],
        BranchKind::Remote => &["refs/remotes/"][..],
    };
    let (local, remote) = query_branches(repo_path, refs)?;
    Ok(match kind {
        BranchKind::Local => local,
        BranchKind::Remote => remote,
    })
}

/// 一次查询本地与远程分支。
pub fn list_all_branches(repo_path: &Path) -> Result<(Vec<Branch>, Vec<Branch>)> {
    query_branches(repo_path, &["refs/heads/", "refs/remotes/"])
}

fn query_branches(repo_path: &Path, refs: &[&str]) -> Result<(Vec<Branch>, Vec<Branch>)> {
    let format_arg = format!("--format={BRANCH_FORMAT}");
    let mut args = vec!["for-each-ref", format_arg.as_str()];
    args.extend_from_slice(refs);
    let bytes = run_git_bytes(repo_path, &args)?;
    parse_branch_records(&bytes).map_err(|error| {
        warn!(
            operation = "git_branch_parse",
            repo = %repo_path.display(),
            ref_count = refs.len(),
            error = %error,
            "git branch output parse failed"
        );
        error
    })
}

fn parse_branch_records(bytes: &[u8]) -> Result<(Vec<Branch>, Vec<Branch>)> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for (record_index, raw) in bytes.split(|byte| *byte == 0x1e).enumerate() {
        let record = raw.strip_prefix(b"\n").unwrap_or(raw);
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if record.is_empty() {
            continue;
        }
        ensure_git_record_size(record, "Git 分支记录", record_index + 1)?;
        let fields = record.split(|byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() != 6 {
            return Err(branch_parse_error(record_index, "字段数量异常"));
        }
        let full_name = decode_branch_field(fields[0], record_index, "ref 名")?;
        let (kind, name) = if let Some(name) = full_name.strip_prefix("refs/heads/") {
            (BranchKind::Local, name)
        } else if let Some(name) = full_name.strip_prefix("refs/remotes/") {
            (BranchKind::Remote, name)
        } else {
            return Err(branch_parse_error(record_index, "ref 前缀异常"));
        };
        if name.is_empty() {
            return Err(branch_parse_error(record_index, "分支名为空"));
        }
        let object_id = decode_branch_field(fields[1], record_index, "commit id")?;
        if object_id.is_empty() || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(branch_parse_error(record_index, "commit id 无效"));
        }
        let head = decode_branch_field(fields[2], record_index, "HEAD 标记")?;
        if head != " " && head != "*" {
            return Err(branch_parse_error(record_index, "HEAD 标记异常"));
        }
        let upstream = decode_branch_field(fields[3], record_index, "upstream")?;
        let track = decode_branch_field(fields[4], record_index, "track 状态")?;
        let symref = decode_branch_field(fields[5], record_index, "symbolic ref")?;
        // 忽略不能操作的符号引用。
        if !symref.is_empty() {
            continue;
        }
        let (upstream, ahead, behind) = if upstream.is_empty() {
            if !track.is_empty() {
                return Err(branch_parse_error(
                    record_index,
                    "无 upstream 但存在 track 状态",
                ));
            }
            (None, None, None)
        } else {
            let (ahead, behind) = parse_track(track, record_index)?;
            (Some(upstream.to_string()), ahead, behind)
        };
        let branch = Branch {
            name: name.to_string(),
            kind,
            commit: CommitId(object_id.to_string()),
            is_head: matches!(kind, BranchKind::Local) && head == "*",
            upstream,
            ahead,
            behind,
        };
        ensure_git_list_room(local.len().saturating_add(remote.len()), "Git 分支列表")?;
        let target = match kind {
            BranchKind::Local => &mut local,
            BranchKind::Remote => &mut remote,
        };
        target.push(branch);
    }
    Ok((local, remote))
}

fn decode_branch_field<'a>(bytes: &'a [u8], index: usize, label: &str) -> Result<&'a str> {
    std::str::from_utf8(bytes)
        .map_err(|error| branch_parse_error(index, &format!("{label} 非 UTF-8：{error}")))
}

fn branch_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git 分支第 {} 条记录失败：{reason}",
        index + 1
    ))
}

fn parse_track(s: &str, line_index: usize) -> Result<(Option<usize>, Option<usize>)> {
    let s = s.trim();
    if s.is_empty() || s == "[gone]" {
        return Ok((None, None));
    }
    let s = s
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| branch_parse_error(line_index, "track 状态缺少方括号"))?;
    let mut ahead = None;
    let mut behind = None;
    for part in s.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            if ahead.is_some() {
                return Err(branch_parse_error(line_index, "ahead 计数重复"));
            }
            ahead = Some(n.trim().parse().map_err(|error| {
                branch_parse_error(line_index, &format!("ahead 计数无效：{error}"))
            })?);
        } else if let Some(n) = part.strip_prefix("behind ") {
            if behind.is_some() {
                return Err(branch_parse_error(line_index, "behind 计数重复"));
            }
            behind = Some(n.trim().parse().map_err(|error| {
                branch_parse_error(line_index, &format!("behind 计数无效：{error}"))
            })?);
        } else {
            return Err(branch_parse_error(line_index, "未知 track 状态"));
        }
    }
    Ok((ahead, behind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_remote_branches_in_one_response() -> Result<()> {
        let raw = b"refs/heads/main\0aaaaaaaa\0*\0origin/main\0[ahead 2, behind 1]\0\x1e\n\
                    refs/remotes/origin/main\0bbbbbbbb\0 \0\0\0\x1e\n\
                    refs/remotes/origin/HEAD\0bbbbbbbb\0 \0\0\0refs/remotes/origin/main\x1e\n";
        let (local, remote) = parse_branch_records(raw)?;

        assert_eq!(local.len(), 1);
        assert!(local[0].is_head);
        assert_eq!(local[0].upstream.as_deref(), Some("origin/main"));
        assert_eq!((local[0].ahead, local[0].behind), (Some(2), Some(1)));
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].name, "origin/main");
        assert!(!remote[0].is_head);
        Ok(())
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
