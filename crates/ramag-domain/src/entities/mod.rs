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
    MAX_CLIPBOARD_BLACKLIST_ENTRIES, MAX_CLIPBOARD_ITEM_BYTES, MAX_CLIPBOARD_SEARCH_BYTES,
    blacklist_matches, classify_text, fnv1a_hash, is_safe_http_url, make_preview,
    normalize_blacklist_source, parse_hex_color,
};
pub use connection::{
    ConnectionConfig, ConnectionId, DriverKind, MAX_CONNECTION_HOST_BYTES,
    MAX_CONNECTION_IDENTIFIER_BYTES, MAX_CONNECTION_NAME_BYTES, MAX_CONNECTION_PASSWORD_BYTES,
    MAX_CONNECTION_PATH_BYTES, MAX_CONNECTION_REMARK_BYTES, MAX_CONNECTION_SSH_TARGET_BYTES,
    TlsVerify,
};
pub use git::{
    BlameLine, Branch, BranchKind, Commit, CommitId, ConflictContent, DiffKind, DiffLine,
    DiffLineKind, FileChangeKind, FileDiff, FileStatus, Hunk, LogOptions, MAX_COMMIT_MESSAGE_BYTES,
    MAX_GIT_NAME_ARG_BYTES, MAX_GIT_PATCH_BYTES, MAX_GIT_PATH_ARGS, MAX_GIT_PATH_ARGS_BYTES,
    MAX_GIT_PATH_BYTES, MAX_GIT_PATH_DEPTH, MAX_GIT_POSITIONAL_ARG_BYTES,
    MAX_GIT_STASH_MESSAGE_BYTES, MAX_GIT_TAG_MESSAGE_BYTES, RebaseAction, RebaseTodo, ReflogEntry,
    Remote, RepoConfig, RepoId, RepoOperation, ResetKind, Signature, Stash, StashId, Tag, TagKind,
    WorkingTreeStatus,
};
pub use history::{
    MAX_QUERY_HISTORY_ERROR_BYTES, MAX_QUERY_HISTORY_SQL_BYTES, QueryHistoryPage, QueryRecord,
    QueryRecordId, QueryStatus, compact_text_preview,
};
pub use mongo::{
    MAX_MONGO_COLLECTION_NAME_BYTES, MAX_MONGO_DATABASE_NAME_BYTES, MAX_MONGO_DOCUMENT_BYTES,
    MAX_MONGO_FIELD_PATH_BYTES, MAX_MONGO_NESTING_DEPTH, MAX_MONGO_PIPELINE_STAGES,
    MAX_MONGO_VALUE_NODES, MongoCollection, MongoCollectionStats, MongoDatabase, MongoDocument,
    MongoIndex, MongoQueryResult, MongoQuerySpec, validate_mongo_collection_name,
    validate_mongo_database_name, validate_mongo_document, validate_mongo_field_path,
    validate_mongo_pipeline,
};
pub use query::{
    MAX_SQL_QUERY_BYTES, Query, QueryResult, Row, Value, Warning, json_pretty_bounded,
};
pub use redis_keyspace::{
    KeyMeta, MAX_REDIS_COLLECTION_BYTES, MAX_REDIS_COLLECTION_ITEMS, MAX_REDIS_COMMAND_ARG_BYTES,
    MAX_REDIS_COMMAND_ARGS, MAX_REDIS_COMMAND_BYTES, MAX_REDIS_COMMAND_NAME_BYTES,
    MAX_REDIS_KEY_BYTES, MAX_REDIS_MATCH_PATTERN_BYTES, MAX_REDIS_SCAN_ALL_KEYS,
    MAX_REDIS_SCAN_COUNT, RedisType, ScanResult, validate_redis_collection_limit,
    validate_redis_command, validate_redis_key, validate_redis_match_pattern,
    validate_redis_scan_count,
};
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
