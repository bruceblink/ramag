//! 查询历史：每条 SQL 不论成败都落库，UI 可回看 / 重跑

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ConnectionId, contains_case_insensitive};

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
    pub status: QueryStatus,
    /// 耗时毫秒，失败为 0
    pub elapsed_ms: u64,
    /// 受影响 / 返回行数
    pub rows: u64,
    pub error: Option<String>,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct QueryHistoryPage {
    pub records: Vec<QueryRecord>,
    pub truncated: bool,
}

impl QueryRecord {
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
        connection_name: impl Into<String>,
        sql: impl Into<String>,
        elapsed_ms: u64,
        rows: u64,
    ) -> Self {
        Self {
            id: QueryRecordId::new(),
            connection_id,
            connection_name: connection_name.into(),
            sql: sql.into(),
            status: QueryStatus::Success,
            elapsed_ms,
            rows,
            error: None,
            executed_at: Utc::now(),
        }
    }

    pub fn new_failed(
        connection_id: ConnectionId,
        connection_name: impl Into<String>,
        sql: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            id: QueryRecordId::new(),
            connection_id,
            connection_name: connection_name.into(),
            sql: sql.into(),
            status: QueryStatus::Failed,
            elapsed_ms: 0,
            rows: 0,
            error: Some(error.into()),
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
    use super::compact_text_preview;
    use crate::entities::contains_case_insensitive;

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
}
