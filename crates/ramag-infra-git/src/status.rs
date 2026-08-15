//! Git 工作区状态与操作检测。

use std::path::Path;

use ramag_domain::entities::{
    FileChangeKind, FileStatus, MAX_INCREMENTAL_STATUS_PATH_BYTES, MAX_INCREMENTAL_STATUS_PATHS,
    RepoOperation, WorkingTreeStatus,
};
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, validate_output_path,
    validate_path_args,
};

mod branches;

pub use branches::{list_all_branches, list_branches};

pub fn collect_status(repo_path: &Path, git_dir: &Path) -> Result<WorkingTreeStatus> {
    let parsed = run_porcelain_v2(repo_path, true, &[])?;

    Ok(WorkingTreeStatus {
        head_branch: parsed.head_branch,
        head_commit: parsed.head_commit,
        operation: detect_operation(git_dir),
        files: parsed.files,
        ahead: parsed.ahead,
        behind: parsed.behind,
    })
}

/// 只查询文件监听报告的路径；不读取 HEAD / 分支，避免普通编辑触发全仓扫描。
pub fn collect_status_paths(repo_path: &Path, paths: &[String]) -> Result<Vec<FileStatus>> {
    validate_incremental_paths(paths)?;
    Ok(run_porcelain_v2(repo_path, false, paths)?.files)
}

#[derive(Default)]
struct ParsedStatus {
    files: Vec<FileStatus>,
    head_branch: Option<String>,
    head_commit: Option<String>,
    ahead: Option<usize>,
    behind: Option<usize>,
    saw_head_branch: bool,
    saw_head_commit: bool,
}

/// 查询并解析 porcelain v2 状态。
fn run_porcelain_v2(
    repo_path: &Path,
    include_branch: bool,
    paths: &[String],
) -> Result<ParsedStatus> {
    let mut args = vec![
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--untracked-files=all".to_string(),
        "-z".to_string(),
    ];
    if include_branch {
        args.push("--branch".into());
    }
    if !paths.is_empty() {
        args.push("--".into());
        args.extend(paths.iter().cloned());
    }
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let bytes = run_git_bytes(repo_path, &args)?;
    let mut parsed = parse_porcelain_bytes(&bytes).map_err(|error| {
        warn!(
            operation = "git_status_parse",
            repo = %repo_path.display(),
            include_branch,
            path_count = paths.len(),
            error = %error,
            "git status output parse failed"
        );
        error
    })?;
    // 路径有序时避免额外排序。
    if !parsed
        .files
        .windows(2)
        .all(|pair| compare_file_status(&pair[0], &pair[1]) != std::cmp::Ordering::Greater)
    {
        parsed.files.sort_unstable_by(compare_file_status);
    }
    Ok(parsed)
}

fn compare_file_status(left: &FileStatus, right: &FileStatus) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.old_path.cmp(&right.old_path))
}

fn validate_incremental_paths(paths: &[String]) -> Result<()> {
    validate_path_args(paths, "增量状态路径")?;
    let total_bytes = paths
        .iter()
        .try_fold(0usize, |total, path| total.checked_add(path.len() + 1))
        .ok_or_else(|| DomainError::InvalidConfig("增量状态路径总长度溢出".into()))?;
    if paths.len() > MAX_INCREMENTAL_STATUS_PATHS || total_bytes > MAX_INCREMENTAL_STATUS_PATH_BYTES
    {
        return Err(DomainError::InvalidConfig(format!(
            "增量状态路径超过 {MAX_INCREMENTAL_STATUS_PATHS} 条或 {} KiB 上限",
            MAX_INCREMENTAL_STATUS_PATH_BYTES / 1024
        )));
    }
    Ok(())
}

fn parse_porcelain_bytes(bytes: &[u8]) -> Result<ParsedStatus> {
    let mut parsed = ParsedStatus::default();
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
            b'#' => parse_branch_header(record, record_index, &mut parsed, &mut ahead_behind)?,
            b'1' => {
                ensure_git_list_room(parsed.files.len(), "Git 工作区文件状态")?;
                parsed.files.push(parse_ordinary(record, record_index)?);
            }
            b'2' => {
                // 下一项是旧路径。
                let old_path = iter
                    .next()
                    .ok_or_else(|| status_parse_error(record_index, "rename 记录缺少旧路径"))?;
                ensure_git_record_size(old_path, "Git 工作区旧路径", record_index)?;
                ensure_git_list_room(parsed.files.len(), "Git 工作区文件状态")?;
                parsed
                    .files
                    .push(parse_rename(record, old_path, record_index)?);
            }
            b'?' => {
                ensure_git_list_room(parsed.files.len(), "Git 工作区文件状态")?;
                parsed.files.push(parse_untracked(record, record_index)?);
            }
            b'u' => {
                ensure_git_list_room(parsed.files.len(), "Git 工作区文件状态")?;
                parsed.files.push(parse_unmerged(record, record_index)?);
            }
            other => {
                return Err(status_parse_error(
                    record_index,
                    &format!("未知记录类型 0x{other:02x}"),
                ));
            }
        }
    }
    (parsed.ahead, parsed.behind) = ahead_behind
        .map(|(ahead, behind)| (Some(ahead), Some(behind)))
        .unwrap_or((None, None));
    parsed
        .files
        .sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(parsed)
}

fn parse_branch_header(
    record: &[u8],
    index: usize,
    parsed: &mut ParsedStatus,
    ahead_behind: &mut Option<(usize, usize)>,
) -> Result<()> {
    let text = decode_status_record(record, index)?;
    if let Some(value) = text.strip_prefix("# branch.oid ") {
        if parsed.saw_head_commit {
            return Err(status_parse_error(index, "branch.oid 重复"));
        }
        parsed.saw_head_commit = true;
        if value != "(initial)" {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(status_parse_error(index, "branch.oid 无效"));
            }
            parsed.head_commit = Some(value.chars().take(7).collect());
        }
        return Ok(());
    }
    if let Some(value) = text.strip_prefix("# branch.head ") {
        if parsed.saw_head_branch {
            return Err(status_parse_error(index, "branch.head 重复"));
        }
        parsed.saw_head_branch = true;
        if value != "(detached)" {
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(status_parse_error(index, "branch.head 无效"));
            }
            parsed.head_branch = Some(value.to_string());
        }
        return Ok(());
    }
    if let Some(value) = text.strip_prefix("# branch.ab ") {
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
        return Ok(());
    }
    // 忽略不影响当前快照的分支头。
    text.starts_with("# ")
        .then_some(())
        .ok_or_else(|| status_parse_error(index, "分支头记录格式异常"))
}

fn parse_branch_count(value: Option<&str>, prefix: char, index: usize) -> Result<usize> {
    value
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| status_parse_error(index, "branch.ab 计数前缀异常"))?
        .parse()
        .map_err(|error| status_parse_error(index, &format!("branch.ab 计数无效：{error}")))
}

fn parse_ordinary(record: &[u8], index: usize) -> Result<FileStatus> {
    // 格式：1 XY sub mH mI mW hH hI path。
    let text = decode_status_record(record, index)?;
    let parts: Vec<&str> = text.splitn(9, ' ').collect();
    if parts.len() != 9 || parts[0] != "1" {
        return Err(status_parse_error(index, "普通记录字段数量异常"));
    }
    let path = parts[8];
    if path.is_empty() {
        return Err(status_parse_error(index, "普通记录路径为空"));
    }
    validate_output_path(path, "Git 工作区路径", index)?;
    let (staged, unstaged) = parse_xy(parts[1], index)?;
    Ok(FileStatus {
        path: path.to_string(),
        old_path: None,
        staged,
        unstaged,
    })
}

fn parse_rename(record: &[u8], old_path: &[u8], index: usize) -> Result<FileStatus> {
    // 格式：2 XY sub mH mI mW hH hI Xscore newpath。
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
    validate_output_path(parts[9], "Git 工作区新路径", index)?;
    validate_output_path(old_path, "Git 工作区旧路径", index)?;
    Ok(FileStatus {
        path: parts[9].to_string(),
        old_path: Some(old_path.to_string()),
        staged,
        unstaged,
    })
}

fn parse_untracked(record: &[u8], index: usize) -> Result<FileStatus> {
    // 格式：? path。
    let text = decode_status_record(record, index)?;
    let path = text
        .strip_prefix("? ")
        .ok_or_else(|| status_parse_error(index, "未跟踪记录前缀异常"))?;
    if path.is_empty() {
        return Err(status_parse_error(index, "未跟踪路径为空"));
    }
    validate_output_path(path, "Git 未跟踪路径", index)?;
    Ok(FileStatus {
        path: path.to_string(),
        old_path: None,
        staged: None,
        unstaged: Some(FileChangeKind::Untracked),
    })
}

fn parse_unmerged(record: &[u8], index: usize) -> Result<FileStatus> {
    // 格式：u XY sub m1 m2 m3 mW h1 h2 h3 path。
    let text = decode_status_record(record, index)?;
    let parts: Vec<&str> = text.splitn(11, ' ').collect();
    if parts.len() != 11 || parts[0] != "u" {
        return Err(status_parse_error(index, "冲突记录字段数量异常"));
    }
    if parts[10].is_empty() {
        return Err(status_parse_error(index, "冲突路径为空"));
    }
    validate_output_path(parts[10], "Git 冲突路径", index)?;
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

fn detect_operation(git_dir: &Path) -> Option<RepoOperation> {
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
        let parsed = parse_porcelain_bytes(raw)?;
        assert_eq!(parsed.files.len(), 4);
        assert_eq!(parsed.files[0].path, "conflict.rs");
        assert_eq!(parsed.files[0].staged, Some(FileChangeKind::Conflicted));
        assert_eq!(parsed.files[1].path, "new file.rs");
        assert_eq!(parsed.files[2].path, "new.rs");
        assert_eq!(parsed.files[2].old_path.as_deref(), Some("old.rs"));
        assert_eq!(parsed.files[3].path, "src/lib.rs");
        assert_eq!((parsed.ahead, parsed.behind), (None, None));
        Ok(())
    }

    #[test]
    fn parses_branch_ahead_behind_without_extra_command() -> Result<()> {
        let raw = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0\
                    # branch.ab +12 -3\0? file.txt\0";
        let parsed = parse_porcelain_bytes(raw)?;
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.head_branch.as_deref(), Some("main"));
        assert_eq!(parsed.head_commit.as_deref(), Some("abc"));
        assert_eq!((parsed.ahead, parsed.behind), (Some(12), Some(3)));
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
}
