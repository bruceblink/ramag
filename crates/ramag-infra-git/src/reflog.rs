//! `git reflog show --format=%H\x1f%gd\x1f%gs\x1f%cI`。action 从 subject 冒号前段抽

use std::path::Path;

use ramag_domain::entities::{CommitId, ReflogEntry};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_text, validate_positional_arg,
};

const REFLOG_FORMAT: &str = "%H%x1f%gd%x1f%gs%x1f%cI";

pub fn list(
    repo_path: &Path,
    ref_name: Option<&str>,
    limit: Option<usize>,
) -> Result<Vec<ReflogEntry>> {
    if let Some(ref_name) = ref_name {
        validate_positional_arg(ref_name, "reflog 引用")?;
    }
    let mut args: Vec<String> = vec![
        "reflog".into(),
        "show".into(),
        format!("--format={REFLOG_FORMAT}"),
    ];
    if let Some(n) = limit {
        args.push(format!("--max-count={n}"));
    }
    args.push(ref_name.unwrap_or("HEAD").to_string());
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let raw = run_git_text(repo_path, &args_ref)?;
    parse_reflog(&raw)
}

fn parse_reflog(text: &str) -> Result<Vec<ReflogEntry>> {
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        ensure_git_list_room(entries.len(), "Git reflog 列表")?;
        ensure_git_record_size(line.as_bytes(), "Git reflog 记录", index + 1)?;
        let mut parts = line.splitn(4, '\x1f');
        let hash = parts.next().unwrap_or_default().trim();
        let selector = parts.next();
        let raw_subject = parts.next();
        let date_iso = parts.next();
        let (Some(selector), Some(raw_subject), Some(date_iso)) = (selector, raw_subject, date_iso)
        else {
            return Err(reflog_parse_error(index, "字段数量不足"));
        };
        if hash.is_empty() || selector.is_empty() {
            return Err(reflog_parse_error(index, "commit id 或 selector 为空"));
        }
        let timestamp = chrono::DateTime::parse_from_rfc3339(date_iso)
            .map(|time| time.with_timezone(&chrono::Utc))
            .map_err(|error| reflog_parse_error(index, &format!("时间格式无效：{error}")))?;
        // 形如 "commit: foo" / "checkout: moving from a to b"
        let (action, subject) = match raw_subject.split_once(':') {
            Some((action, subject)) => (action.trim().to_string(), subject.trim().to_string()),
            None => (String::new(), raw_subject.to_string()),
        };
        entries.push(ReflogEntry {
            commit: CommitId(hash.to_string()),
            selector: selector.to_string(),
            action,
            subject,
            timestamp,
        });
    }
    Ok(entries)
}

fn reflog_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git reflog 第 {} 条记录失败：{reason}",
        index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_reflog() -> Result<()> {
        let text = "abc123\u{1f}HEAD@{0}\u{1f}commit: fix bug\u{1f}2026-01-01T12:00:00+00:00\n\
                    def456\u{1f}HEAD@{1}\u{1f}checkout: moving from main to feature\u{1f}2026-01-01T11:00:00+00:00\n";
        let entries = parse_reflog(text)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].commit.0, "abc123");
        assert_eq!(entries[0].selector, "HEAD@{0}");
        assert_eq!(entries[0].action, "commit");
        assert_eq!(entries[0].subject, "fix bug");
        assert_eq!(entries[1].action, "checkout");
        assert_eq!(entries[1].subject, "moving from main to feature");
        Ok(())
    }

    #[test]
    fn handles_subject_without_colon() -> Result<()> {
        let text = "abc\u{1f}HEAD@{0}\u{1f}initial\u{1f}2026-01-01T00:00:00+00:00\n";
        let entries = parse_reflog(text)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].action.is_empty());
        assert_eq!(entries[0].subject, "initial");
        Ok(())
    }

    #[test]
    fn empty_input() -> Result<()> {
        assert_eq!(parse_reflog("")?.len(), 0);
        Ok(())
    }

    #[test]
    fn malformed_reflog_is_reported() {
        let text = "abc\u{1f}HEAD@{0}\u{1f}commit: bad\u{1f}invalid-date\n";
        assert!(parse_reflog(text).is_err());
    }
}
