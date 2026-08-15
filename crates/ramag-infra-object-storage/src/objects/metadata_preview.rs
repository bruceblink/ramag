//! 对象元数据读取。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use opendal::{Metadata, Operator};
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES, ObjectMetadata, ObjectTextPreview, is_opendal_safe_key,
};
use ramag_domain::error::ObjectStorageResult;

use crate::errors::{invalid, map_opendal};

const MAX_USER_METADATA_ENTRIES: usize = 256;
const MAX_USER_METADATA_FIELD_BYTES: usize = 4 * 1024;

pub async fn stat(operator: Arc<Operator>, key: &str) -> ObjectStorageResult<ObjectMetadata> {
    ensure_safe_key(key, "stat")?;
    let metadata = operator
        .stat(key)
        .await
        .map_err(|error| map_opendal("stat", error))?;
    to_metadata(key, &metadata)
}

pub async fn read_text_preview(
    operator: Arc<Operator>,
    key: &str,
) -> ObjectStorageResult<ObjectTextPreview> {
    ensure_safe_key(key, "preview")?;
    let metadata = operator
        .stat(key)
        .await
        .map_err(|error| map_opendal("preview", error))?;
    if !supports_text_preview(key, metadata.content_type()) {
        return Err(invalid(
            "preview",
            "当前文件格式不支持内容查看，请下载后打开",
        ));
    }
    let total_bytes = metadata.content_length();
    let read_bytes = total_bytes.min((MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES + 1) as u64);
    let bytes = if read_bytes == 0 {
        bytes::Bytes::new()
    } else {
        operator
            .reader(key)
            .await
            .map_err(|error| map_opendal("preview", error))?
            .read(0..read_bytes)
            .await
            .map_err(|error| map_opendal("preview", error))?
            .to_bytes()
    };
    let visible_len = bytes.len().min(MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES);
    let visible = &bytes[..visible_len];
    if visible.contains(&0) {
        return Err(invalid("preview", "当前文件不是可预览的文本，请下载后打开"));
    }
    let content = std::str::from_utf8(visible)
        .map_err(|_| invalid("preview", "当前文件不是 UTF-8 文本，请下载后打开"))?
        .to_string();
    Ok(ObjectTextPreview {
        content,
        total_bytes,
        truncated: total_bytes > MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES as u64,
    })
}

fn supports_text_preview(key: &str, content_type: Option<&str>) -> bool {
    if content_type.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("text/")
            || value.contains("json")
            || value.contains("xml")
            || value.contains("yaml")
    }) {
        return true;
    }
    let extension = key
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some(
            "txt"
                | "log"
                | "json"
                | "jsonl"
                | "md"
                | "csv"
                | "xml"
                | "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "conf"
                | "cfg"
                | "rs"
                | "py"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "css"
                | "html"
                | "htm"
                | "sql"
                | "sh"
                | "zsh"
                | "bash"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "step"
                | "stp"
        )
    )
}

fn to_metadata(key: &str, metadata: &Metadata) -> ObjectStorageResult<ObjectMetadata> {
    let mut user_metadata: Vec<(String, String)> = metadata
        .user_metadata()
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    if user_metadata.len() > MAX_USER_METADATA_ENTRIES
        || user_metadata
            .iter()
            .any(|(key, value)| !is_safe_metadata_field(key) || !is_safe_metadata_field(value))
        || [metadata.etag(), metadata.version(), metadata.content_type()]
            .into_iter()
            .flatten()
            .any(|value| !is_safe_metadata_field(value))
    {
        return Err(invalid("stat", "对象自定义元数据超过安全上限"));
    }
    user_metadata.sort();
    Ok(ObjectMetadata {
        key: key.to_string(),
        size: metadata.content_length(),
        last_modified: metadata.last_modified().and_then(parse_time),
        etag: metadata.etag().map(str::to_string),
        version: metadata.version().map(str::to_string),
        content_type: metadata.content_type().map(str::to_string),
        user_metadata,
        storage_class: None,
    })
}

fn is_safe_metadata_field(value: &str) -> bool {
    value.len() <= MAX_USER_METADATA_FIELD_BYTES
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{061c}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
}

fn ensure_safe_key(key: &str, operation: &'static str) -> ObjectStorageResult<()> {
    if is_opendal_safe_key(key) {
        Ok(())
    } else {
        Err(invalid(operation, "当前对象键无法由 OpenDAL 安全表示"))
    }
}

fn parse_time(value: impl ToString) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value.to_string())
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::supports_text_preview;

    #[test]
    fn preview_accepts_common_text_and_rejects_binary_files() {
        assert!(supports_text_preview("config.json", None));
        assert!(supports_text_preview("solver.log", None));
        assert!(supports_text_preview("model.step", None));
        assert!(supports_text_preview(
            "without-extension",
            Some("text/plain")
        ));
        assert!(!supports_text_preview("archive.zip", None));
        assert!(!supports_text_preview("result.odb", None));
        assert!(!supports_text_preview("video.mp4", Some("video/mp4")));
    }
}
