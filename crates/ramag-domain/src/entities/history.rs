//! 查询历史：每条 SQL 不论成败都落库，UI 可回看 / 重跑

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ConnectionId, MAX_CONNECTION_NAME_BYTES, contains_case_insensitive};

/// 历史正文只承担回看与常规重放；超大脚本保留有界前缀并显式标记，避免单条记录膨胀。
pub const MAX_QUERY_HISTORY_SQL_BYTES: usize = 1024 * 1024;
/// 服务端错误可能携带大段语句 / 文档，只保留足够排查的前缀。
pub const MAX_QUERY_HISTORY_ERROR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueryRecordId(pub Uuid);

impl QueryRecordId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for QueryRecordId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for QueryRecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRecord {
    pub id: QueryRecordId,
    pub connection_id: ConnectionId,
    /// 连接显示名快照（连接删除后仍可识别）
    pub connection_name: String,
    pub sql: String,
    /// true 表示历史只保留正文前缀，UI 不得把不完整内容用于复制或重跑。
    #[serde(default)]
    pub sql_truncated: bool,
    pub status: QueryStatus,
    /// 耗时毫秒，失败为 0
    pub elapsed_ms: u64,
    /// 受影响 / 返回行数
    pub rows: u64,
    pub error: Option<String>,
    #[serde(default)]
    pub error_truncated: bool,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct QueryHistoryPage {
    pub records: Vec<QueryRecord>,
    pub truncated: bool,
}

impl QueryRecord {
    /// 兼容旧记录与自定义 Storage：加载 / 保存前原地收紧到当前资源边界。
    pub fn enforce_limits(&mut self) {
        truncate_owned(&mut self.connection_name, MAX_CONNECTION_NAME_BYTES);
        self.sql_truncated |= truncate_owned(&mut self.sql, MAX_QUERY_HISTORY_SQL_BYTES);
        if let Some(error) = &mut self.error {
            self.error_truncated |= truncate_owned(error, MAX_QUERY_HISTORY_ERROR_BYTES);
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_len(
            "历史连接名称",
            &self.connection_name,
            MAX_CONNECTION_NAME_BYTES,
        )?;
        validate_len("历史正文", &self.sql, MAX_QUERY_HISTORY_SQL_BYTES)?;
        if let Some(error) = &self.error {
            validate_len("历史错误", error, MAX_QUERY_HISTORY_ERROR_BYTES)?;
        }
        Ok(())
    }

    pub fn inline_payload_bytes(&self) -> u64 {
        let bytes = self
            .connection_name
            .len()
            .saturating_add(self.sql.len())
            .saturating_add(self.error.as_ref().map_or(0, String::len));
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    pub fn new_success(
        connection_id: ConnectionId,
        connection_name: impl AsRef<str>,
        sql: impl AsRef<str>,
        elapsed_ms: u64,
        rows: u64,
    ) -> Self {
        let (connection_name, _) =
            bounded_text(connection_name.as_ref(), MAX_CONNECTION_NAME_BYTES);
        let (sql, sql_truncated) = bounded_text(sql.as_ref(), MAX_QUERY_HISTORY_SQL_BYTES);
        Self {
            id: QueryRecordId::new(),
            connection_id,
            connection_name,
            sql,
            sql_truncated,
            status: QueryStatus::Success,
            elapsed_ms,
            rows,
            error: None,
            error_truncated: false,
            executed_at: Utc::now(),
        }
    }

    pub fn new_failed(
        connection_id: ConnectionId,
        connection_name: impl AsRef<str>,
        sql: impl AsRef<str>,
        error: impl AsRef<str>,
    ) -> Self {
        let (connection_name, _) =
            bounded_text(connection_name.as_ref(), MAX_CONNECTION_NAME_BYTES);
        let (sql, sql_truncated) = bounded_text(sql.as_ref(), MAX_QUERY_HISTORY_SQL_BYTES);
        let (error, error_truncated) = bounded_text(error.as_ref(), MAX_QUERY_HISTORY_ERROR_BYTES);
        Self {
            id: QueryRecordId::new(),
            connection_id,
            connection_name,
            sql,
            sql_truncated,
            status: QueryStatus::Failed,
            elapsed_ms: 0,
            rows: 0,
            error: Some(error),
            error_truncated,
            executed_at: Utc::now(),
        }
    }

    /// SQL 单行预览：去多余空白 + 截断
    pub fn sql_preview(&self, max_chars: usize) -> String {
        compact_text_preview(&self.sql, max_chars)
    }

    /// 历史搜索匹配 SQL 与错误文本；ASCII 路径不分配整段小写副本。
    pub fn matches_query_lower(&self, query_lower: &str) -> bool {
        contains_case_insensitive(&self.sql, query_lower)
            || self
                .error
                .as_deref()
                .is_some_and(|error| contains_case_insensitive(error, query_lower))
    }
}

fn validate_len(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!(
            "{label}过长：{} bytes，最多 {max_bytes} bytes",
            value.len()
        ));
    }
    Ok(())
}

fn bounded_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let end = floor_char_boundary(value, max_bytes);
    (value[..end].to_string(), true)
}

fn truncate_owned(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let end = floor_char_boundary(value, max_bytes);
    value.truncate(end);
    value.shrink_to_fit();
    true
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 压平连续空白并按字符数截断；达到上限即停止扫描，避免仅为短预览复制整段长文本。
pub fn compact_text_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::with_capacity(text.len().min(max_chars));
    let mut remaining = max_chars;
    let mut has_word = false;
    let mut truncated = false;

    'words: for word in text.split_whitespace() {
        if has_word {
            if remaining == 0 {
                truncated = true;
                break;
            }
            preview.push(' ');
            remaining -= 1;
        }
        for character in word.chars() {
            if remaining == 0 {
                truncated = true;
                break 'words;
            }
            preview.push(character);
            remaining -= 1;
        }
        has_word = true;
    }
    if truncated {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_QUERY_HISTORY_ERROR_BYTES, MAX_QUERY_HISTORY_SQL_BYTES, QueryRecord,
        compact_text_preview,
    };
    use crate::entities::{ConnectionId, contains_case_insensitive};

    #[test]
    fn compact_preview_flattens_whitespace_and_stops_at_limit() {
        assert_eq!(compact_text_preview("a\n  b\t c", 20), "a b c");
        assert_eq!(compact_text_preview("abcdef", 3), "abc…");
        assert_eq!(compact_text_preview("数据库查询失败", 3), "数据库…");
        assert_eq!(compact_text_preview("abc", 3), "abc");
        assert_eq!(compact_text_preview("abc", 0), "…");
        assert_eq!(compact_text_preview("   ", 0), "");
    }

    #[test]
    fn history_search_is_case_insensitive_without_losing_unicode() {
        assert!(contains_case_insensitive("SELECT UserName", "username"));
        assert!(contains_case_insensitive("查询数据库", "数据库"));
        assert!(!contains_case_insensitive("SELECT 1", "update"));
    }

    #[test]
    fn history_constructors_bound_text_and_preserve_unicode_boundaries() {
        let sql = "中".repeat(MAX_QUERY_HISTORY_SQL_BYTES / 3 + 1);
        let error = "错".repeat(MAX_QUERY_HISTORY_ERROR_BYTES / 3 + 1);
        let record = QueryRecord::new_failed(ConnectionId::new(), "local", &sql, &error);

        assert_eq!(
            record.sql.len(),
            MAX_QUERY_HISTORY_SQL_BYTES - MAX_QUERY_HISTORY_SQL_BYTES % 3
        );
        assert!(record.sql_truncated);
        assert_eq!(
            record.error.as_ref().map(String::len),
            Some(MAX_QUERY_HISTORY_ERROR_BYTES - MAX_QUERY_HISTORY_ERROR_BYTES % 3)
        );
        assert!(record.error_truncated);
        assert!(record.validate().is_ok());
    }

    #[test]
    fn legacy_history_is_normalized_in_place() {
        let mut record = QueryRecord::new_success(ConnectionId::new(), "local", "SELECT 1", 1, 1);
        record.sql = "中".repeat(MAX_QUERY_HISTORY_SQL_BYTES / 3 + 2);
        record.sql_truncated = false;
        record.error = Some("e".repeat(MAX_QUERY_HISTORY_ERROR_BYTES + 1));
        record.error_truncated = false;

        record.enforce_limits();

        assert!(record.sql.len() <= MAX_QUERY_HISTORY_SQL_BYTES);
        assert!(record.sql_truncated);
        assert!(record.error_truncated);
        assert!(record.validate().is_ok());
    }
}
