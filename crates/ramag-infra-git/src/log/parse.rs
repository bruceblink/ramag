//! Git 日志文本解析。

use ramag_domain::entities::{Commit, CommitId, Signature};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{ensure_git_list_room, ensure_git_message_size};

pub(super) const MAX_COMMIT_PARENTS: usize = 1024;
pub(super) const MAX_COMMIT_REFS: usize = 256;

pub(super) fn parse_log_list_output(text: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for (index, record) in text.split('\x1e').enumerate() {
        let trimmed = record.trim_start_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        ensure_git_list_room(commits.len(), "Git 日志列表")?;
        ensure_git_message_size(trimmed.as_bytes(), "Git 日志记录", index + 1)?;
        let commit = parse_log_list_record(trimmed).map_err(|reason| {
            DomainError::QueryFailed(format!(
                "解析 Git 日志第 {} 条记录失败：{reason}",
                index + 1
            ))
        })?;
        commits.push(commit);
    }
    Ok(commits)
}

pub(super) fn parse_log_list_record(record: &str) -> std::result::Result<Commit, String> {
    let mut fields = record.splitn(6, '\x1f');
    let hash = fields.next().ok_or("缺少 commit id")?.trim();
    if hash.is_empty() {
        return Err("commit id 为空".into());
    }
    let author_name = fields.next().ok_or("缺少作者名")?;
    let author_ts = fields
        .next()
        .ok_or("缺少作者时间")?
        .parse::<i64>()
        .map_err(|error| format!("作者时间非整数：{error}"))?;
    let parents = parse_parents(fields.next().ok_or("缺少父提交字段")?)?;
    let refs = parse_refs(fields.next().ok_or("缺少引用字段")?);
    let subject = fields.next().ok_or("缺少提交主题")?.to_string();
    let author_timestamp = chrono::DateTime::from_timestamp(author_ts, 0)
        .ok_or_else(|| format!("作者时间超出支持范围：{author_ts}"))?;

    Ok(Commit {
        id: CommitId(hash.to_string()),
        parents,
        author: Signature {
            name: author_name.to_string(),
            email: String::new(),
            timestamp: author_timestamp,
        },
        // 列表不读取 committer；详情页按需读取。
        committer: Signature {
            name: String::new(),
            email: String::new(),
            timestamp: author_timestamp,
        },
        subject,
        body: String::new(),
        refs,
    })
}

pub(super) fn parse_log_output(text: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for (index, record) in text.split('\x1e').enumerate() {
        let trimmed = record.trim_start_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        ensure_git_list_room(commits.len(), "Git 日志列表")?;
        ensure_git_message_size(trimmed.as_bytes(), "Git 日志记录", index + 1)?;
        let commit = parse_record(trimmed).map_err(|reason| {
            DomainError::QueryFailed(format!(
                "解析 Git 日志第 {} 条记录失败：{reason}",
                index + 1
            ))
        })?;
        commits.push(commit);
    }
    Ok(commits)
}

pub(super) fn parse_record(record: &str) -> std::result::Result<Commit, String> {
    let mut fields = record.splitn(11, '\x1f');
    let hash = fields.next().ok_or("缺少 commit id")?.trim();
    if hash.is_empty() {
        return Err("commit id 为空".into());
    }
    let author_name = fields.next().ok_or("缺少作者名")?;
    let author_email = fields.next().ok_or("缺少作者邮箱")?;
    let author_ts = fields
        .next()
        .ok_or("缺少作者时间")?
        .parse::<i64>()
        .map_err(|error| format!("作者时间非整数：{error}"))?;
    let committer_name = fields.next().ok_or("缺少提交者名")?;
    let committer_email = fields.next().ok_or("缺少提交者邮箱")?;
    let committer_ts = fields
        .next()
        .ok_or("缺少提交时间")?
        .parse::<i64>()
        .map_err(|error| format!("提交时间非整数：{error}"))?;
    let parents_str = fields.next().ok_or("缺少父提交字段")?;
    // %D 的 refs 以逗号分隔。
    let refs = parse_refs(fields.next().ok_or("缺少引用字段")?);
    let subject = fields.next().ok_or("缺少提交主题")?.to_string();
    let body = fields
        .next()
        .ok_or("缺少提交正文字段")?
        .trim_end_matches('\n')
        .to_string();

    let author_timestamp = chrono::DateTime::from_timestamp(author_ts, 0)
        .ok_or_else(|| format!("作者时间超出支持范围：{author_ts}"))?;
    let committer_timestamp = chrono::DateTime::from_timestamp(committer_ts, 0)
        .ok_or_else(|| format!("提交时间超出支持范围：{committer_ts}"))?;

    let parents = parse_parents(parents_str)?;

    Ok(Commit {
        id: CommitId(hash.to_string()),
        parents,
        author: Signature {
            name: author_name.to_string(),
            email: author_email.to_string(),
            timestamp: author_timestamp,
        },
        committer: Signature {
            name: committer_name.to_string(),
            email: committer_email.to_string(),
            timestamp: committer_timestamp,
        },
        subject,
        body,
        refs,
    })
}

fn parse_parents(raw: &str) -> std::result::Result<Vec<CommitId>, String> {
    let mut parents = Vec::new();
    for parent in raw.split_whitespace().filter(|value| !value.is_empty()) {
        if parents.len() >= MAX_COMMIT_PARENTS {
            return Err(format!("父提交数量超过 {MAX_COMMIT_PARENTS} 个安全上限"));
        }
        parents.push(CommitId(parent.to_string()));
    }
    Ok(parents)
}

pub(super) fn parse_refs(raw: &str) -> Vec<String> {
    let mut iter = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut refs: Vec<String> = iter
        .by_ref()
        .take(MAX_COMMIT_REFS)
        .map(str::to_string)
        .collect();
    let remaining = iter.count();
    if remaining > 0 {
        // 为省略提示保留一个位置。
        refs.pop();
        refs.push(format!("…另有 {} 个引用已省略", remaining + 1));
    }
    refs
}
