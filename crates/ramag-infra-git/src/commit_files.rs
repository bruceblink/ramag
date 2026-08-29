//! commit 涉及的文件列表，走 `git diff-tree --name-status`。`staged` 承载类型，`unstaged` 永远 None

use std::path::Path;

use ramag_domain::entities::{FileChangeKind, FileStatus};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, validate_output_path,
    validate_positional_arg,
};

pub fn list(repo_path: &Path, commit: &str) -> Result<Vec<FileStatus>> {
    validate_positional_arg(commit, "commit id")?;
    let raw = run_git_bytes(
        repo_path,
        // --root：根 commit（无父）与空树对比，否则 diff-tree 对根 commit 返回空
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-status",
            "-z",
            "-r",
            commit,
        ],
    )?;
    parse_diff_tree(&raw)
}

/// 读取两个 revision 之间的文件清单，供分支比较面板复用 commit 文件树模型。
pub fn list_range(repo_path: &Path, from: &str, to: &str) -> Result<Vec<FileStatus>> {
    validate_positional_arg(from, "diff 起点")?;
    validate_positional_arg(to, "diff 终点")?;
    let raw = run_git_bytes(
        repo_path,
        &[
            "diff",
            "--find-renames",
            "--name-status",
            "-z",
            from,
            to,
            "--",
        ],
    )?;
    parse_name_status(&raw, "Git 范围文件列表")
}

fn parse_diff_tree(raw: &[u8]) -> Result<Vec<FileStatus>> {
    parse_name_status(raw, "Git commit 文件列表")
}

fn parse_name_status(raw: &[u8], context: &str) -> Result<Vec<FileStatus>> {
    let mut out = Vec::new();
    let mut fields = raw
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut record_index = 0;
    while let Some(code_raw) = fields.next() {
        record_index += 1;
        ensure_git_list_room(out.len(), context)?;
        ensure_git_record_size(code_raw, &format!("{context}状态码"), record_index)?;
        let code_raw = std::str::from_utf8(code_raw).map_err(|error| {
            status_parse_error(context, record_index, &format!("状态码非 UTF-8：{error}"))
        })?;
        // R/C 后跟相似度数字，仅取首字母
        let code = code_raw
            .chars()
            .next()
            .ok_or_else(|| status_parse_error(context, record_index, "状态码为空"))?;
        let kind = match code {
            'M' => FileChangeKind::Modified,
            'A' => FileChangeKind::Added,
            'D' => FileChangeKind::Deleted,
            'R' => FileChangeKind::Renamed,
            'C' => FileChangeKind::Copied,
            'T' => FileChangeKind::TypeChanged,
            _ => {
                return Err(status_parse_error(
                    context,
                    record_index,
                    &format!("未知状态码：{code_raw}"),
                ));
            }
        };
        validate_status_code(code_raw, kind, record_index, context)?;

        let first_path = fields
            .next()
            .ok_or_else(|| status_parse_error(context, record_index, "缺少文件路径"))?;
        ensure_git_record_size(first_path, &format!("{context}文件路径"), record_index)?;
        let first_path = decode_diff_path(first_path, record_index, context)?;
        let (path, old_path) = match kind {
            FileChangeKind::Renamed | FileChangeKind::Copied => {
                let new_path = fields
                    .next()
                    .ok_or_else(|| status_parse_error(context, record_index, "缺少新文件路径"))?;
                ensure_git_record_size(new_path, &format!("{context}新文件路径"), record_index)?;
                (
                    decode_diff_path(new_path, record_index, context)?.to_string(),
                    Some(first_path.to_string()),
                )
            }
            _ => (first_path.to_string(), None),
        };
        out.push(FileStatus {
            path,
            old_path,
            staged: Some(kind),
            unstaged: None,
        });
    }
    Ok(out)
}

fn validate_status_code(
    code: &str,
    kind: FileChangeKind,
    index: usize,
    context: &str,
) -> Result<()> {
    let valid = match kind {
        FileChangeKind::Renamed | FileChangeKind::Copied => {
            code.len() > 1 && code[1..].bytes().all(|byte| byte.is_ascii_digit())
        }
        _ => code.len() == 1,
    };
    if valid {
        Ok(())
    } else {
        Err(status_parse_error(context, index, "状态码格式异常"))
    }
}

fn decode_diff_path<'a>(path: &'a [u8], index: usize, context: &str) -> Result<&'a str> {
    if path.is_empty() {
        return Err(status_parse_error(context, index, "文件路径为空"));
    }
    let path = std::str::from_utf8(path).map_err(|error| {
        status_parse_error(context, index, &format!("文件路径非 UTF-8：{error}"))
    })?;
    validate_output_path(path, &format!("{context}文件路径"), index)?;
    Ok(path)
}

fn status_parse_error(context: &str, index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!("解析 {context}第 {index} 条记录失败：{reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_modify() -> Result<()> {
        let raw = b"M\0src/lib.rs\0A\0src/new.rs\0D\0src/old.rs\0";
        let files = parse_diff_tree(raw)?;
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].staged, Some(FileChangeKind::Modified));
        assert_eq!(files[1].staged, Some(FileChangeKind::Added));
        assert_eq!(files[2].staged, Some(FileChangeKind::Deleted));
        Ok(())
    }

    #[test]
    fn parses_rename_with_old_path() -> Result<()> {
        let raw = b"R100\0old.rs\0new.rs\0";
        let files = parse_diff_tree(raw)?;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new.rs");
        assert_eq!(files[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[0].staged, Some(FileChangeKind::Renamed));
        Ok(())
    }

    #[test]
    fn accepts_paths_with_tabs_and_newlines() -> Result<()> {
        let files = parse_diff_tree(b"M\0dir/a\tb\nc.rs\0")?;
        assert_eq!(files[0].path, "dir/a\tb\nc.rs");
        Ok(())
    }

    #[test]
    fn malformed_diff_tree_is_reported() {
        assert!(parse_diff_tree(b"M\0").is_err());
        assert!(parse_diff_tree(b"R100\0old.rs\0").is_err());
        assert!(parse_diff_tree(b"X\0file.rs\0").is_err());
        assert!(parse_diff_tree(b"M\0\xff\0").is_err());
    }
}
