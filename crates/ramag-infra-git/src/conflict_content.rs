//! 三方冲突内容：`git show :N:<path>`，1=base、2=ours、3=theirs

use std::path::Path;

use ramag_domain::entities::ConflictContent;
use ramag_domain::error::{DomainError, Result};

use crate::git_cmd::{ensure_git_list_room, ensure_git_record_size, run_git_bytes};

const MAX_CONFLICT_STAGE_BYTES: usize = 4 * 1024 * 1024;

/// stage 不存在（add/delete 冲突）时对应侧返回空 Vec
pub fn get_content(repo_path: &Path, file_path: &str) -> Result<ConflictContent> {
    let stages_raw = run_git_bytes(repo_path, &["ls-files", "-u", "-z", "--", file_path])?;
    let stages = parse_stages(&stages_raw)?;
    if stages.is_empty() {
        return Err(DomainError::InvalidConfig(format!(
            "文件当前没有未解决的冲突 stage：{file_path}"
        )));
    }
    Ok(ConflictContent {
        path: file_path.to_string(),
        base: read_stage(repo_path, &stages, 1, file_path)?,
        ours: read_stage(repo_path, &stages, 2, file_path)?,
        theirs: read_stage(repo_path, &stages, 3, file_path)?,
    })
}

fn parse_stages(raw: &[u8]) -> Result<std::collections::HashSet<u8>> {
    let mut stages = std::collections::HashSet::new();
    for (index, record) in raw.split(|byte| *byte == 0).enumerate() {
        if record.is_empty() {
            continue;
        }
        if stages.len() >= 3 {
            return Err(stage_parse_error(index, "冲突 stage 记录超过 3 条"));
        }
        ensure_git_record_size(record, "Git 冲突索引记录", index + 1)?;
        let mut fields = record.splitn(2, |byte| *byte == b'\t');
        let metadata = fields.next().unwrap_or_default();
        if fields.next().is_none() {
            return Err(stage_parse_error(index, "缺少路径分隔符"));
        }
        let metadata_fields: Vec<&[u8]> = metadata
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect();
        if metadata_fields.len() != 3 {
            return Err(stage_parse_error(index, "元数据字段数量异常"));
        }
        let stage_text = std::str::from_utf8(metadata_fields[2])
            .map_err(|error| stage_parse_error(index, &format!("stage 非 UTF-8：{error}")))?;
        let stage = stage_text
            .parse::<u8>()
            .map_err(|error| stage_parse_error(index, &format!("stage 非数字：{error}")))?;
        if !(1..=3).contains(&stage) {
            return Err(stage_parse_error(index, "stage 必须为 1、2 或 3"));
        }
        if !stages.insert(stage) {
            return Err(stage_parse_error(index, "同一 stage 重复"));
        }
    }
    Ok(stages)
}

fn stage_parse_error(index: usize, reason: &str) -> DomainError {
    DomainError::QueryFailed(format!("解析冲突索引第 {} 条记录失败：{reason}", index + 1))
}

fn read_stage(
    repo_path: &Path,
    stages: &std::collections::HashSet<u8>,
    stage: u8,
    file_path: &str,
) -> Result<Vec<String>> {
    if !stages.contains(&stage) {
        return Ok(Vec::new());
    }
    let spec = format!(":{stage}:{file_path}");
    let bytes = run_git_bytes(repo_path, &["show", &spec])?;
    decode_stage_content(&bytes, stage)
}

fn decode_stage_content(bytes: &[u8], stage: u8) -> Result<Vec<String>> {
    if bytes.len() > MAX_CONFLICT_STAGE_BYTES {
        return Err(DomainError::QueryFailed(format!(
            "Git 冲突文件 stage {stage} 超过 {} MiB 预览上限，请在外部编辑器处理",
            MAX_CONFLICT_STAGE_BYTES / 1024 / 1024
        )));
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        ensure_git_list_room(lines.len(), "Git 冲突文件行列表")?;
        ensure_git_record_size(line.as_bytes(), "Git 冲突文件行", index + 1)?;
        lines.push(line.to_string());
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_present_conflict_stages() -> Result<()> {
        let raw = b"100644 abc 1\tfile.txt\0\
                    100644 def 3\tfile.txt\0";
        let stages = parse_stages(raw)?;
        assert!(stages.contains(&1));
        assert!(!stages.contains(&2));
        assert!(stages.contains(&3));
        Ok(())
    }

    #[test]
    fn malformed_conflict_stage_is_not_treated_as_absent() {
        assert!(parse_stages(b"100644 abc x\tfile.txt\0").is_err());
        assert!(parse_stages(b"100644 abc 2 file.txt\0").is_err());
        assert!(parse_stages(b"100644 abc 4\tfile.txt\0").is_err());
        assert!(parse_stages(b"100644 abc 2\tfile.txt\x00100644 def 2\tfile.txt\0").is_err());
    }

    #[test]
    fn conflict_stage_content_is_bounded() {
        assert!(decode_stage_content(b"one\ntwo\n", 2).is_ok());
        assert!(decode_stage_content(&vec![b'x'; MAX_CONFLICT_STAGE_BYTES + 1], 2).is_err());
    }
}
