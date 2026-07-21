//! `git blame --porcelain` 解析。同 sha 的后续 group 仅 sha 头 + `\t<content>`，metadata 缓存复用

use std::collections::HashMap;
use std::path::Path;

use ramag_domain::entities::{BlameLine, CommitId};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_text, validate_path_arg,
};

pub fn run(repo_path: &Path, file: &str) -> Result<Vec<BlameLine>> {
    validate_path_arg(file, "blame 文件路径")?;
    let raw = run_git_text(repo_path, &["blame", "--porcelain", "--", file])?;
    parse_porcelain(&raw)
}

#[derive(Debug, Default, Clone)]
struct CommitMeta {
    author: Option<String>,
    timestamp: Option<i64>,
    subject: Option<String>,
}

fn parse_porcelain(text: &str) -> Result<Vec<BlameLine>> {
    let mut metas: HashMap<String, CommitMeta> = HashMap::new();
    let mut out: Vec<BlameLine> = Vec::new();
    let mut lines = text.lines();
    let mut input_line_index = 0;
    while let Some(header) = lines.next() {
        input_line_index += 1;
        ensure_git_list_room(out.len(), "Git blame 行列表")?;
        ensure_git_record_size(header.as_bytes(), "Git blame 输入行", input_line_index)?;
        // 头行：<sha> <orig> <final> [<count>]
        let mut parts = header.split_whitespace();
        let sha = parts
            .next()
            .ok_or_else(|| blame_parse_error(out.len(), "头记录缺少 commit id"))?;
        let original_line = parts
            .next()
            .ok_or_else(|| blame_parse_error(out.len(), "头记录缺少原始行号"))?;
        let final_line = parts
            .next()
            .ok_or_else(|| blame_parse_error(out.len(), "头记录缺少最终行号"))?;
        original_line
            .parse::<u32>()
            .map_err(|error| blame_parse_error(out.len(), &format!("原始行号无效：{error}")))?;
        let final_line = final_line
            .parse::<u32>()
            .map_err(|error| blame_parse_error(out.len(), &format!("最终行号无效：{error}")))?;
        if final_line == 0 {
            return Err(blame_parse_error(out.len(), "最终行号必须大于 0"));
        }
        if out
            .last()
            .is_some_and(|previous| previous.line_no >= final_line)
        {
            return Err(blame_parse_error(out.len(), "最终行号未严格递增"));
        }
        if let Some(count) = parts.next() {
            count
                .parse::<u32>()
                .map_err(|error| blame_parse_error(out.len(), &format!("行数无效：{error}")))?;
        }
        if parts.next().is_some() {
            return Err(blame_parse_error(out.len(), "头记录字段过多"));
        }

        // 已知 sha 取 cached；新 sha 用下面的 metadata 行填充
        let mut meta: CommitMeta = metas.get(sha).cloned().unwrap_or_default();
        let mut content = None;
        for line in lines.by_ref() {
            input_line_index += 1;
            ensure_git_record_size(line.as_bytes(), "Git blame 输入行", input_line_index)?;
            if let Some(c) = line.strip_prefix('\t') {
                // \t 是该行实际内容，结束本 group
                content = Some(c.to_string());
                break;
            }
            let mut kv = line.splitn(2, ' ');
            let key = kv.next().unwrap_or_default();
            let val = kv.next().unwrap_or("");
            match key {
                "author" => meta.author = Some(val.to_string()),
                "author-time" => {
                    meta.timestamp = Some(val.parse().map_err(|error| {
                        blame_parse_error(out.len(), &format!("作者时间无效：{error}"))
                    })?);
                }
                "summary" => meta.subject = Some(val.to_string()),
                "author-mail" | "author-tz" | "committer" | "committer-mail" | "committer-time"
                | "committer-tz" | "boundary" | "previous" | "filename" | "encoding" => {}
                _ => {
                    return Err(blame_parse_error(
                        out.len(),
                        &format!("未知 metadata 字段：{key}"),
                    ));
                }
            }
        }
        let content = content.ok_or_else(|| blame_parse_error(out.len(), "缺少源码内容行"))?;
        let author = meta
            .author
            .clone()
            .ok_or_else(|| blame_parse_error(out.len(), "缺少作者信息"))?;
        let timestamp_raw = meta
            .timestamp
            .ok_or_else(|| blame_parse_error(out.len(), "缺少作者时间"))?;
        let subject = meta
            .subject
            .clone()
            .ok_or_else(|| blame_parse_error(out.len(), "缺少提交主题"))?;
        let timestamp = chrono::DateTime::from_timestamp(timestamp_raw, 0).ok_or_else(|| {
            blame_parse_error(out.len(), &format!("作者时间超出支持范围：{timestamp_raw}"))
        })?;
        metas.insert(sha.to_string(), meta.clone());
        out.push(BlameLine {
            commit: CommitId(sha.to_string()),
            author,
            timestamp,
            line_no: final_line,
            subject,
            content,
        });
    }
    Ok(out)
}

fn blame_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git blame 第 {} 条记录失败：{reason}",
        index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_lines_same_commit() -> Result<()> {
        let text = "\
abc123 1 1 2
author Alice
author-mail <a@x.com>
author-time 1700000000
author-tz +0000
committer Alice
committer-mail <a@x.com>
committer-time 1700000000
committer-tz +0000
summary first commit
filename foo.rs
\tline one
abc123 2 2
\tline two
";
        let blame = parse_porcelain(text)?;
        assert_eq!(blame.len(), 2);
        assert_eq!(blame[0].commit.0, "abc123");
        assert_eq!(blame[0].author, "Alice");
        assert_eq!(blame[0].subject, "first commit");
        assert_eq!(blame[0].content, "line one");
        assert_eq!(blame[0].line_no, 1);
        assert_eq!(blame[1].commit.0, "abc123");
        assert_eq!(blame[1].author, "Alice");
        assert_eq!(blame[1].content, "line two");
        assert_eq!(blame[1].line_no, 2);
        Ok(())
    }

    #[test]
    fn empty_input() -> Result<()> {
        assert_eq!(parse_porcelain("")?.len(), 0);
        Ok(())
    }

    #[test]
    fn malformed_blame_is_reported() {
        assert!(parse_porcelain("abc123 1 bad\n\tline\n").is_err());
        assert!(parse_porcelain("abc123 1 1\nauthor Alice\n").is_err());
        assert!(
            parse_porcelain("abc123 1 1\nauthor Alice\nauthor-time bad\nsummary x\n\tline\n")
                .is_err()
        );
        assert!(
            parse_porcelain(
                "abc123 1 2\nauthor Alice\nauthor-time 1700000000\nsummary x\n\tline 2\n\
                 abc123 2 1\n\tline 1\n"
            )
            .is_err()
        );
    }
}
