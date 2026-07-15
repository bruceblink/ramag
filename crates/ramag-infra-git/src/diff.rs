//! `git diff` unified 输出 → FileDiff。binary 仅识别占位行不渲染；mode 字段留空

use std::path::Path;

use ramag_domain::entities::{
    CommitId, DiffKind, DiffLine, DiffLineKind, FileChangeKind, FileDiff, Hunk,
};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, validate_positional_arg,
};

pub fn run_diff(repo_path: &Path, path: &str, kind: &DiffKind) -> Result<FileDiff> {
    run_diff_opts(repo_path, path, kind, false)
}

pub fn run_diff_opts(
    repo_path: &Path,
    path: &str,
    kind: &DiffKind,
    ignore_whitespace: bool,
) -> Result<FileDiff> {
    run_diff_full_opts(repo_path, path, kind, ignore_whitespace, 3)
}

/// `context_lines`：3=标准、0=仅变更、999999=全文件
pub fn run_diff_full_opts(
    repo_path: &Path,
    path: &str,
    kind: &DiffKind,
    ignore_whitespace: bool,
    context_lines: u32,
) -> Result<FileDiff> {
    let mut args_strings = build_diff_args(path, kind, context_lines)?;
    if ignore_whitespace {
        args_strings.insert(1, "-w".into());
    }
    let args: Vec<&str> = args_strings.iter().map(String::as_str).collect();
    let bytes = run_git_bytes(repo_path, &args)?;
    let text = String::from_utf8(bytes).map_err(|error| {
        DomainError::QueryFailed(format!(
            "Git diff 包含非 UTF-8 文本，当前版本无法安全执行行级操作：{error}"
        ))
    })?;
    parse_unified_diff(&text, path)
}

fn build_diff_args(path: &str, kind: &DiffKind, context_lines: u32) -> Result<Vec<String>> {
    // CommitVsParent 走 diff-tree --root：根 commit（无父）与空树对比，
    // 否则 `git diff <c>^ <c>` 对根 commit 因 `<c>^` 不存在而报错（点第一个 commit 看 diff 会失败）
    if let DiffKind::CommitVsParent(CommitId(c)) = kind {
        validate_positional_arg(c, "diff commit")?;
        return Ok(vec![
            "diff-tree".into(),
            "--no-color".into(),
            format!("-U{context_lines}"),
            "--find-renames".into(),
            "--root".into(),
            "-p".into(),
            "--no-commit-id".into(),
            c.clone(),
            "--".into(),
            path.into(),
        ]);
    }
    let mut args: Vec<String> = vec![
        "diff".into(),
        "--no-color".into(),
        format!("-U{context_lines}"),
        "--find-renames".into(),
    ];
    match kind {
        DiffKind::WorkingTreeVsIndex => {}
        DiffKind::IndexVsHead => args.push("--cached".into()),
        DiffKind::WorkingTreeVsHead => args.push("HEAD".into()),
        DiffKind::CommitVsParent(_) => unreachable!("已在函数开头用 diff-tree 处理"),
        DiffKind::Range {
            from: CommitId(f),
            to: CommitId(t),
        } => {
            validate_positional_arg(f, "diff 起点")?;
            validate_positional_arg(t, "diff 终点")?;
            args.push(f.clone());
            args.push(t.clone());
        }
    }
    args.push("--".into());
    args.push(path.into());
    Ok(args)
}

fn parse_unified_diff(text: &str, path: &str) -> Result<FileDiff> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<Hunk> = None;
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;
    let mut binary = false;
    let mut change_kind = FileChangeKind::Modified;
    let mut old_path: Option<String> = None;
    let mut parsed_items = 0;

    // split_terminator 仅移除 `\n`，保留正文可能存在的 `\r`。
    for (line_index, line) in text.split_terminator('\n').enumerate() {
        ensure_git_record_size(line.as_bytes(), "Git diff 行", line_index + 1)?;
        if line.starts_with("Binary files") {
            binary = true;
            continue;
        }
        if line.starts_with("new file") {
            change_kind = FileChangeKind::Added;
            continue;
        }
        if line.starts_with("deleted file") {
            change_kind = FileChangeKind::Deleted;
            continue;
        }
        if line.starts_with("rename from ") {
            change_kind = FileChangeKind::Renamed;
            old_path = Some(line.trim_start_matches("rename from ").to_string());
            continue;
        }
        if line.starts_with("@@") {
            ensure_git_list_room(parsed_items, "Git diff 实体")?;
            parsed_items += 1;
            if let Some(h) = current.take() {
                push_validated_hunk(h, &mut hunks)?;
            }
            // `@@ -os[,ol] +ns[,nl] @@ heading`
            let header = line
                .strip_prefix("@@ ")
                .and_then(|value| value.split_once(" @@"))
                .ok_or_else(|| diff_parse_error(line_index, "hunk 头格式无效"))?;
            let ranges: Vec<&str> = header.0.split_whitespace().collect();
            if ranges.len() != 2 {
                return Err(diff_parse_error(line_index, "hunk 范围字段数量异常"));
            }
            let old_range = ranges[0]
                .strip_prefix('-')
                .ok_or_else(|| diff_parse_error(line_index, "旧范围缺少 '-' 前缀"))?;
            let new_range = ranges[1]
                .strip_prefix('+')
                .ok_or_else(|| diff_parse_error(line_index, "新范围缺少 '+' 前缀"))?;
            let (os, ol) = parse_range(old_range, line_index)?;
            let (ns, nl) = parse_range(new_range, line_index)?;
            let heading = match header.1.trim() {
                "" => None,
                value => Some(value.to_string()),
            };
            old_no = os;
            new_no = ns;
            current = Some(Hunk {
                old_start: os,
                old_lines: ol,
                new_start: ns,
                new_lines: nl,
                heading,
                lines: Vec::new(),
            });
            continue;
        }
        if line.starts_with("diff --git") || line.starts_with("index ") {
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        let Some(h) = current.as_mut() else { continue };
        match line.chars().next() {
            Some(' ') => {
                push_diff_line(
                    h,
                    &mut parsed_items,
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_lineno: Some(old_no),
                        new_lineno: Some(new_no),
                        text: line[1..].to_string(),
                    },
                )?;
                old_no = next_line_number(old_no, line_index)?;
                new_no = next_line_number(new_no, line_index)?;
            }
            Some('-') => {
                push_diff_line(
                    h,
                    &mut parsed_items,
                    DiffLine {
                        kind: DiffLineKind::Delete,
                        old_lineno: Some(old_no),
                        new_lineno: None,
                        text: line[1..].to_string(),
                    },
                )?;
                old_no = next_line_number(old_no, line_index)?;
            }
            Some('+') => {
                push_diff_line(
                    h,
                    &mut parsed_items,
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_lineno: None,
                        new_lineno: Some(new_no),
                        text: line[1..].to_string(),
                    },
                )?;
                new_no = next_line_number(new_no, line_index)?;
            }
            // `\ No newline at end of file` 等忽略
            Some('\\') if line == "\\ No newline at end of file" => {}
            _ => {
                return Err(diff_parse_error(line_index, "hunk 内出现未知行类型"));
            }
        }
    }
    if let Some(h) = current {
        push_validated_hunk(h, &mut hunks)?;
    }

    Ok(FileDiff {
        path: path.to_string(),
        old_path,
        change_kind,
        binary,
        old_mode: None,
        new_mode: None,
        hunks,
    })
}

fn push_diff_line(hunk: &mut Hunk, parsed_items: &mut usize, line: DiffLine) -> Result<()> {
    ensure_git_list_room(*parsed_items, "Git diff 实体")?;
    *parsed_items += 1;
    hunk.lines.push(line);
    Ok(())
}

fn parse_range(s: &str, line_index: usize) -> Result<(u32, u32)> {
    let (start, count) = match s.split_once(',') {
        Some((a, b)) => (a, b),
        None => (s, "1"),
    };
    let start = start
        .parse()
        .map_err(|error| diff_parse_error(line_index, &format!("起始行号无效：{error}")))?;
    let count = count
        .parse()
        .map_err(|error| diff_parse_error(line_index, &format!("行数无效：{error}")))?;
    Ok((start, count))
}

fn next_line_number(current: u32, line_index: usize) -> Result<u32> {
    current
        .checked_add(1)
        .ok_or_else(|| diff_parse_error(line_index, "行号超出支持范围"))
}

fn push_validated_hunk(hunk: Hunk, hunks: &mut Vec<Hunk>) -> Result<()> {
    let actual_old = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, DiffLineKind::Context | DiffLineKind::Delete))
        .count();
    let actual_new = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, DiffLineKind::Context | DiffLineKind::Add))
        .count();
    if actual_old != hunk.old_lines as usize || actual_new != hunk.new_lines as usize {
        return Err(DomainError::QueryFailed(format!(
            "解析 Git diff 失败：hunk 声明 {}/{} 行，实际 {}/{} 行",
            hunk.old_lines, hunk.new_lines, actual_old, actual_new
        )));
    }
    hunks.push(hunk);
    Ok(())
}

fn diff_parse_error(line_index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git diff 第 {} 行失败：{reason}",
        line_index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_modify_diff() -> Result<()> {
        let text = "\
diff --git a/file.txt b/file.txt
index abc..def 100644
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,4 @@
 line1
-old
+new
+added
 line3
";
        let d = parse_unified_diff(text, "file.txt")?;
        assert_eq!(d.path, "file.txt");
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.old_lines, 3);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.new_lines, 4);
        assert_eq!(h.lines.len(), 5);
        let kinds: Vec<DiffLineKind> = h.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Context,
                DiffLineKind::Delete,
                DiffLineKind::Add,
                DiffLineKind::Add,
                DiffLineKind::Context,
            ]
        );
        Ok(())
    }

    #[test]
    fn parses_new_file() -> Result<()> {
        let text = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 000..abc
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let d = parse_unified_diff(text, "new.txt")?;
        assert_eq!(d.change_kind, FileChangeKind::Added);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].lines.len(), 2);
        Ok(())
    }

    #[test]
    fn preserves_carriage_return_in_content() -> Result<()> {
        let text = "@@ -1 +1 @@\n-old\r\n+new\r\n";
        let diff = parse_unified_diff(text, "file.txt")?;
        assert_eq!(diff.hunks[0].lines[0].text, "old\r");
        assert_eq!(diff.hunks[0].lines[1].text, "new\r");
        Ok(())
    }

    #[test]
    fn malformed_hunk_is_reported() {
        assert!(parse_unified_diff("@@ bad @@\n+line\n", "file.txt").is_err());
        assert!(parse_unified_diff("@@ -1,2 +1 @@\n-old\n+new\n", "file.txt").is_err());
        assert!(parse_unified_diff("@@ -x +1 @@\n+new\n", "file.txt").is_err());
    }
}
