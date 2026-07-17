//! `git log --pretty=format:...`：`\x1f` 分字段、`\x1e` 分记录。
//! 字段：%H %an %ae %at %cn %ce %ct %P %D %s %b

use std::path::Path;

use ramag_domain::entities::{Commit, CommitId, LogOptions, Signature};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_message_size, run_git_probe, run_git_text, validate_path_arg,
    validate_positional_arg,
};

const LOG_FORMAT: &str = "%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%P%x1f%D%x1f%s%x1f%b%x1e";
/// 正常 octopus merge 远低于此值；异常父边数量会放大提交图的内存与渲染成本。
const MAX_COMMIT_PARENTS: usize = 1024;
/// 引用只是装饰标签，超限时保留有界前缀并给出省略提示，不阻断历史浏览。
const MAX_COMMIT_REFS: usize = 256;

pub fn run_log(repo_path: &Path, opts: &LogOptions) -> Result<Vec<Commit>> {
    if let Some(start) = &opts.start {
        validate_positional_arg(start, "日志起点")?;
    }
    if let Some(path) = &opts.path_filter {
        validate_path_arg(path, "日志文件路径")?;
    }
    for (label, value) in [
        ("日志关键词", opts.grep.as_deref()),
        ("日志作者", opts.author.as_deref()),
        ("日志时间范围", opts.since.as_deref()),
    ] {
        if let Some(value) = value {
            validate_log_filter(value, label)?;
        }
    }
    if opts
        .limit
        .is_some_and(|limit| limit > crate::git_cmd::MAX_PARSED_GIT_ITEMS)
    {
        return Err(DomainError::InvalidConfig(format!(
            "日志单页数量超过 {} 条安全上限",
            crate::git_cmd::MAX_PARSED_GIT_ITEMS
        )));
    }
    // 新初始化仓库没有 HEAD；这是正常空态，不应把 git log 的 fatal 暴露给用户。
    if opts.start.is_none()
        && !run_git_probe(repo_path, &["rev-parse", "--verify", "--quiet", "HEAD"])?
    {
        return Ok(Vec::new());
    }
    let mut args: Vec<String> = vec!["log".into(), format!("--pretty=format:{LOG_FORMAT}")];
    if opts.skip > 0 {
        args.push(format!("--skip={}", opts.skip));
    }
    if let Some(n) = opts.limit {
        args.push(format!("--max-count={n}"));
    }
    if let Some(g) = &opts.grep {
        args.push(format!("--grep={g}"));
        // git log 默认对 --grep 大小写敏感，UI 期望忽略
        args.push("--regexp-ignore-case".into());
    }
    if let Some(a) = &opts.author {
        args.push(format!("--author={a}"));
    }
    if let Some(s) = &opts.since {
        args.push(format!("--since={s}"));
    }
    if let Some(start) = &opts.start {
        args.push(start.clone());
    }
    if let Some(p) = &opts.path_filter {
        args.push("--".into());
        args.push(p.clone());
    }
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_git_text(repo_path, &args_ref)?;
    parse_log_output(&out)
}

fn validate_log_filter(value: &str, label: &str) -> Result<()> {
    if value.len() > ramag_domain::entities::MAX_GIT_POSITIONAL_ARG_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidConfig(format!(
            "{label}超过 {} KiB 上限或包含控制字符",
            ramag_domain::entities::MAX_GIT_POSITIONAL_ARG_BYTES / 1024
        )));
    }
    Ok(())
}

fn parse_log_output(text: &str) -> Result<Vec<Commit>> {
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

fn parse_record(record: &str) -> std::result::Result<Commit, String> {
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
    // %D：decorate refs（"HEAD -> main, origin/main, tag: v1.0"），逗号分隔
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

    let mut parents = Vec::new();
    for parent in parents_str
        .split_whitespace()
        .filter(|value| !value.is_empty())
    {
        if parents.len() >= MAX_COMMIT_PARENTS {
            return Err(format!("父提交数量超过 {MAX_COMMIT_PARENTS} 个安全上限"));
        }
        parents.push(CommitId(parent.to_string()));
    }

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

fn parse_refs(raw: &str) -> Vec<String> {
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
        // 给提示腾一个位置，最终 Vec 始终不超过 MAX_COMMIT_REFS。
        refs.pop();
        refs.push(format!("…另有 {} 个引用已省略", remaining + 1));
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_records() -> Result<()> {
        let raw = "abc123\x1fAlice\x1falice@x.com\x1f1700000000\x1fAlice\x1falice@x.com\x1f1700000000\x1f\x1fHEAD -> main, tag: v1.0\x1ffirst commit\x1f\x1edef456\x1fBob\x1fbob@x.com\x1f1700001000\x1fBob\x1fbob@x.com\x1f1700001000\x1fabc123\x1f\x1ffix bug\x1ffull body\x1e";
        let commits = parse_log_output(raw)?;
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].id.0, "abc123");
        assert_eq!(commits[0].subject, "first commit");
        assert_eq!(commits[0].author.name, "Alice");
        assert_eq!(commits[0].refs, vec!["HEAD -> main", "tag: v1.0"]);
        assert_eq!(commits[1].parents.len(), 1);
        assert_eq!(commits[1].parents[0].0, "abc123");
        assert_eq!(commits[1].body, "full body");
        assert!(commits[1].refs.is_empty());
        Ok(())
    }

    #[test]
    fn empty_input() -> Result<()> {
        assert_eq!(parse_log_output("")?.len(), 0);
        Ok(())
    }

    #[test]
    fn malformed_record_is_reported() {
        let raw = "abc123\x1fAlice\x1falice@x.com\x1fnot-a-time\x1e";
        assert!(parse_log_output(raw).is_err());
    }

    #[test]
    fn pathological_parent_count_is_rejected() {
        let parents = (0..=MAX_COMMIT_PARENTS)
            .map(|index| format!("{index:040x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let raw = format!(
            "abc123\x1fA\x1fa@x\x1f1700000000\x1fA\x1fa@x\x1f1700000000\x1f{parents}\x1f\x1fsubject\x1f"
        );

        assert!(parse_record(&raw).is_err());
    }

    #[test]
    fn excessive_refs_are_truncated_with_a_hint() {
        let raw = (0..(MAX_COMMIT_REFS + 3))
            .map(|index| format!("ref-{index}"))
            .collect::<Vec<_>>()
            .join(",");

        let refs = parse_refs(&raw);

        assert_eq!(refs.len(), MAX_COMMIT_REFS);
        assert!(refs.last().is_some_and(|value| value.contains("省略")));
    }

    #[test]
    fn non_repository_error_is_not_treated_as_empty_history()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        assert!(run_log(temp.path(), &LogOptions::default()).is_err());
        Ok(())
    }
}
