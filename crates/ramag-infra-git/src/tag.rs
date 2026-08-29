//! Tag 列表 + 创建。`for-each-ref` 同格式覆盖 lightweight（commit）与 annotated（tag）

use std::path::Path;

use ramag_domain::entities::{CommitId, MAX_GIT_TAG_MESSAGE_BYTES, Signature, Tag, TagKind};
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{
    ensure_git_list_room, ensure_git_record_size, run_git_bytes, run_git_stdin, run_git_text,
    validate_name_arg, validate_positional_arg,
};

/// message=Some 走 annotated；sign=true 隐含 annotated
pub fn create(
    repo_path: &Path,
    name: &str,
    target: Option<&str>,
    message: Option<&str>,
    sign: bool,
) -> Result<()> {
    validate_name_arg(name, "tag 名")?;
    if let Some(target) = target {
        validate_positional_arg(target, "tag 目标")?;
    }
    if let Some(message) = message {
        validate_message(message)?;
    }
    let mut args: Vec<&str> = vec!["tag"];
    let placeholder_msg: String;
    let stdin_message: Option<&str>;
    if sign {
        // message=None 时给占位 subject 避免弹编辑器
        args.push("-s");
        stdin_message = match message {
            Some(m) => {
                args.push("-F");
                args.push("-");
                Some(m)
            }
            None => {
                placeholder_msg = format!("Tag {name}");
                args.push("-m");
                args.push(placeholder_msg.as_str());
                None
            }
        };
    } else {
        // 显式覆盖 tag.gpgSign，确保轻量 tag 不受用户全局签名配置影响。
        args.push("--no-sign");
        if let Some(m) = message {
            args.push("-a");
            args.push("-F");
            args.push("-");
            stdin_message = Some(m);
        } else {
            stdin_message = None;
        }
    }
    args.push(name);
    if let Some(t) = target {
        args.push(t);
    }
    match stdin_message {
        Some(message) => run_git_stdin(repo_path, &args, message),
        None => run_git_bytes(repo_path, &args).map(|_| ()),
    }
}

pub(crate) fn validate_message(message: &str) -> Result<()> {
    if message.len() > MAX_GIT_TAG_MESSAGE_BYTES || message.contains('\0') {
        return Err(DomainError::InvalidConfig(format!(
            "tag 备注超过 {} KiB 上限或包含 NUL 字符",
            MAX_GIT_TAG_MESSAGE_BYTES / 1024
        )));
    }
    Ok(())
}

pub fn delete(repo_path: &Path, name: &str) -> Result<()> {
    validate_name_arg(name, "tag 名")?;
    run_git_bytes(repo_path, &["tag", "-d", name]).map(|_| ())
}

pub fn push(repo_path: &Path, remote: &str, name: &str) -> Result<()> {
    validate_name_arg(remote, "远程名")?;
    validate_name_arg(name, "tag 名")?;
    let refname = format!("refs/tags/{name}");
    run_git_bytes(repo_path, &["push", remote, &refname]).map(|_| ())
}

pub fn list(repo_path: &Path) -> Result<Vec<Tag>> {
    // NUL 分字段避开 message 里的换行/tab；LF 分行
    let fmt = "%(refname:short)%00%(objecttype)%00%(objectname)%00\
               %(*objectname)%00%(taggername)%00%(taggeremail)%00\
               %(taggerdate:iso-strict)%00%(*subject)%00%(subject)";
    let format_arg = format!("--format={fmt}");
    let raw = run_git_text(repo_path, &["for-each-ref", &format_arg, "refs/tags/"])?;
    parse_tags(&raw)
}

fn parse_tags(text: &str) -> Result<Vec<Tag>> {
    let mut out = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        ensure_git_list_room(out.len(), "Git tag 列表")?;
        ensure_git_record_size(line.as_bytes(), "Git tag 记录", line_index + 1)?;
        let parts: Vec<&str> = line.split('\u{0}').collect();
        if parts.len() != 9 {
            return Err(tag_parse_error(line_index, "字段数量异常"));
        }
        if parts[0].is_empty() || parts[2].is_empty() {
            return Err(tag_parse_error(line_index, "名称或对象 id 为空"));
        }
        let name = parts[0].to_string();
        let object_type = parts[1];
        let objectname = parts[2];
        let starobjectname = parts[3];
        let tagger_name = parts[4];
        let tagger_email = parts[5].trim_matches(|c| c == '<' || c == '>');
        let tagger_date = parts[6];
        let star_subject = parts[7];
        let plain_subject = parts[8];

        let (kind, commit_hash, message, tagger) = if object_type == "tag" {
            let commit = if starobjectname.is_empty() {
                objectname.to_string()
            } else {
                starobjectname.to_string()
            };
            // 优先 tag 自己 message，空时 fallback 到 commit subject
            let msg = if !plain_subject.is_empty() {
                Some(plain_subject.to_string())
            } else if !star_subject.is_empty() {
                Some(star_subject.to_string())
            } else {
                None
            };
            let sig = parse_signature(tagger_name, tagger_email, tagger_date)
                .map_err(|reason| tag_parse_error(line_index, &reason))?;
            (TagKind::Annotated, commit, msg, sig)
        } else {
            // lightweight：objectname 即 commit
            let msg = if plain_subject.is_empty() {
                None
            } else {
                Some(plain_subject.to_string())
            };
            (TagKind::Lightweight, objectname.to_string(), msg, None)
        };

        out.push(Tag {
            name,
            kind,
            commit: CommitId(commit_hash),
            message,
            tagger,
        });
    }
    Ok(out)
}

fn parse_signature(
    name: &str,
    email: &str,
    date_iso: &str,
) -> std::result::Result<Option<Signature>, String> {
    if name.is_empty() && email.is_empty() && date_iso.is_empty() {
        return Ok(None);
    }
    let timestamp = chrono::DateTime::parse_from_rfc3339(date_iso)
        .map(|t| t.with_timezone(&chrono::Utc))
        .map_err(|error| format!("tagger 时间格式无效：{error}"))?;
    Ok(Some(Signature {
        name: name.to_string(),
        email: email.to_string(),
        timestamp,
    }))
}

fn tag_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!(
        "解析 Git tag 第 {} 条记录失败：{reason}",
        index + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lightweight_with_commit_subject() -> Result<()> {
        let text = "v1.0\u{0}commit\u{0}abc\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}fix bug\n";
        let tags = parse_tags(text)?;
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Lightweight);
        assert_eq!(tags[0].message.as_deref(), Some("fix bug"));
        assert!(tags[0].tagger.is_none());
        Ok(())
    }

    #[test]
    fn parses_annotated_with_tag_message() -> Result<()> {
        let text = "v2.0\u{0}tag\u{0}def\u{0}abc\u{0}Alice\u{0}<a@e.com>\u{0}2026-01-01T00:00:00+00:00\u{0}raw\u{0}release\n";
        let tags = parse_tags(text)?;
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].kind, TagKind::Annotated);
        assert_eq!(tags[0].commit.0, "abc");
        assert_eq!(tags[0].message.as_deref(), Some("release"));
        let Some(sig) = tags[0].tagger.as_ref() else {
            return Err(DomainError::Other("annotated tag 缺少 tagger".into()));
        };
        assert_eq!(sig.name, "Alice");
        assert_eq!(sig.email, "a@e.com");
        Ok(())
    }

    #[test]
    fn skips_empty_lines() -> Result<()> {
        assert!(parse_tags("\n\n")?.is_empty());
        Ok(())
    }

    #[test]
    fn invalid_tagger_date_is_reported() {
        let text = "v2.0\u{0}tag\u{0}def\u{0}abc\u{0}Alice\u{0}<a@e.com>\u{0}bad-date\u{0}raw\u{0}release\n";
        assert!(parse_tags(text).is_err());
    }

    #[test]
    fn tag_message_has_explicit_argument_boundary() {
        assert!(validate_message(&"m".repeat(MAX_GIT_TAG_MESSAGE_BYTES)).is_ok());
        assert!(validate_message(&"m".repeat(MAX_GIT_TAG_MESSAGE_BYTES + 1)).is_err());
        assert!(validate_message("bad\0message").is_err());
    }
}
