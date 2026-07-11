//! `git log --pretty=format:...`：`\x1f` 分字段、`\x1e` 分记录。
//! 字段：%H %an %ae %at %cn %ce %ct %P %D %s %b

use std::path::Path;

use ramag_domain::entities::{Commit, CommitId, LogOptions, Signature};
use ramag_domain::error::Result;

use crate::git_cmd::{run_git_bytes, run_git_text};

const LOG_FORMAT: &str = "%H%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%P%x1f%D%x1f%s%x1f%b%x1e";

pub fn run_log(repo_path: &Path, opts: &LogOptions) -> Result<Vec<Commit>> {
    // 新初始化仓库没有 HEAD；这是正常空态，不应把 git log 的 fatal 暴露给用户。
    if opts.start.is_none() && run_git_bytes(repo_path, &["rev-parse", "--verify", "HEAD"]).is_err()
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
    Ok(parse_log_output(&out))
}

fn parse_log_output(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|r| {
            let trimmed = r.trim_start_matches('\n');
            if trimmed.is_empty() {
                None
            } else {
                parse_record(trimmed)
            }
        })
        .collect()
}

fn parse_record(record: &str) -> Option<Commit> {
    let mut fields = record.splitn(11, '\x1f');
    let hash = fields.next()?.trim();
    let author_name = fields.next()?;
    let author_email = fields.next()?;
    let author_ts = fields.next()?.parse::<i64>().ok()?;
    let committer_name = fields.next()?;
    let committer_email = fields.next()?;
    let committer_ts = fields.next()?.parse::<i64>().ok()?;
    let parents_str = fields.next()?;
    // %D：decorate refs（"HEAD -> main, origin/main, tag: v1.0"），逗号分隔
    let refs: Vec<String> = fields
        .next()?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let subject = fields.next()?.to_string();
    let body = fields
        .next()
        .unwrap_or("")
        .trim_end_matches('\n')
        .to_string();

    let parents = parents_str
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|p| CommitId(p.to_string()))
        .collect();

    Some(Commit {
        id: CommitId(hash.to_string()),
        parents,
        author: Signature {
            name: author_name.to_string(),
            email: author_email.to_string(),
            timestamp: chrono::DateTime::from_timestamp(author_ts, 0).unwrap_or_default(),
        },
        committer: Signature {
            name: committer_name.to_string(),
            email: committer_email.to_string(),
            timestamp: chrono::DateTime::from_timestamp(committer_ts, 0).unwrap_or_default(),
        },
        subject,
        body,
        refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_records() {
        let raw = "abc123\x1fAlice\x1falice@x.com\x1f1700000000\x1fAlice\x1falice@x.com\x1f1700000000\x1f\x1fHEAD -> main, tag: v1.0\x1ffirst commit\x1f\x1edef456\x1fBob\x1fbob@x.com\x1f1700001000\x1fBob\x1fbob@x.com\x1f1700001000\x1fabc123\x1f\x1ffix bug\x1ffull body\x1e";
        let commits = parse_log_output(raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].id.0, "abc123");
        assert_eq!(commits[0].subject, "first commit");
        assert_eq!(commits[0].author.name, "Alice");
        assert_eq!(commits[0].refs, vec!["HEAD -> main", "tag: v1.0"]);
        assert_eq!(commits[1].parents.len(), 1);
        assert_eq!(commits[1].parents[0].0, "abc123");
        assert_eq!(commits[1].body, "full body");
        assert!(commits[1].refs.is_empty());
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse_log_output("").len(), 0);
    }
}
