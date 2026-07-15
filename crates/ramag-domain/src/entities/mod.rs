//! 领域实体：纯 Rust 数据结构 + serde。

pub mod clipboard;
pub mod connection;
pub mod git;
pub mod history;
pub mod mongo;
pub mod query;
pub mod redis_keyspace;
pub mod redis_value;
pub mod schema;

pub use clipboard::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSearchResult, ClipSource, ClipboardSettings,
    blacklist_matches, classify_text, fnv1a_hash, is_safe_http_url, make_preview,
    normalize_blacklist_source, parse_hex_color,
};
pub use connection::{ConnectionConfig, ConnectionId, DriverKind, TlsVerify};
pub use git::{
    BlameLine, Branch, BranchKind, Commit, CommitId, ConflictContent, DiffKind, DiffLine,
    DiffLineKind, FileChangeKind, FileDiff, FileStatus, Hunk, LogOptions, RebaseAction, RebaseTodo,
    ReflogEntry, Remote, RepoConfig, RepoId, RepoOperation, ResetKind, Signature, Stash, StashId,
    Tag, TagKind, WorkingTreeStatus,
};
pub use history::{
    QueryHistoryPage, QueryRecord, QueryRecordId, QueryStatus, compact_text_preview,
};
pub use mongo::{
    MongoCollection, MongoCollectionStats, MongoDatabase, MongoDocument, MongoIndex,
    MongoQueryResult, MongoQuerySpec,
};
pub use query::{Query, QueryResult, Row, Value, Warning};
pub use redis_keyspace::{KeyMeta, RedisType, ScanResult};
pub use redis_value::{RedisValue, RedisValueLoad, StreamEntry};
pub use schema::{Column, ColumnKind, ColumnType, ForeignKey, Index, Schema, Table};

/// 在已小写的查询词下做不区分大小写匹配；ASCII 常见路径不分配整段小写副本。
pub fn contains_case_insensitive(text: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    // ASCII 查询不会命中 UTF-8 多字节中的高位字节，可直接逐字节比较；
    // 即使正文含 Unicode，也无需为整段正文分配 lowercase 副本。
    if query_lower.is_ascii() {
        let query = query_lower.as_bytes();
        return text
            .as_bytes()
            .windows(query.len())
            .any(|window| window.eq_ignore_ascii_case(query));
    }
    text.to_lowercase().contains(query_lower)
}

#[cfg(test)]
mod tests {
    use super::contains_case_insensitive;

    #[test]
    fn ascii_query_matches_unicode_text_without_changing_semantics() {
        assert!(contains_case_insensitive("前缀 Hello 世界", "hello"));
        assert!(!contains_case_insensitive("前缀 世界", "hello"));
        assert!(contains_case_insensitive("前缀 ÜBER", "über"));
    }
}
