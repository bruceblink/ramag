//! 领域实体：纯 Rust 数据结构 + serde。

pub mod clipboard;
pub mod connection;
pub mod ddl;
pub mod git;
pub mod history;
pub mod mongo;
pub mod query;
pub mod redis_keyspace;
pub mod redis_value;
pub mod resource_limits;
pub mod schema;
pub mod ssh;
pub mod transfer;

pub use clipboard::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSearchResult, ClipSource, ClipboardSettings,
    MAX_CLIPBOARD_BLACKLIST_ENTRIES, MAX_CLIPBOARD_ITEM_BYTES, MAX_CLIPBOARD_SEARCH_BYTES,
    blacklist_matches, classify_text, fnv1a_hash, is_safe_http_url, make_preview,
    normalize_blacklist_source, parse_hex_color,
};
pub use connection::{
    ConnectionConfig, ConnectionId, DriverKind, MAX_CONNECTION_CONFIGS,
    MAX_CONNECTION_ENVIRONMENT_BYTES, MAX_CONNECTION_HOST_BYTES, MAX_CONNECTION_IDENTIFIER_BYTES,
    MAX_CONNECTION_NAME_BYTES, MAX_CONNECTION_PASSWORD_BYTES, MAX_CONNECTION_PATH_BYTES,
    MAX_CONNECTION_REMARK_BYTES, MAX_CONNECTION_SSH_TARGET_BYTES, TlsVerify,
};
pub use ddl::build_ddl_query;
pub use git::{
    BlameLine, Branch, BranchKind, Commit, CommitId, ConflictContent, DiffKind, DiffLine,
    DiffLineKind, FileChangeKind, FileDiff, FileStatus, Hunk, LogOptions, MAX_COMMIT_MESSAGE_BYTES,
    MAX_GIT_NAME_ARG_BYTES, MAX_GIT_PATCH_BYTES, MAX_GIT_PATH_ARGS, MAX_GIT_PATH_ARGS_BYTES,
    MAX_GIT_PATH_BYTES, MAX_GIT_PATH_DEPTH, MAX_GIT_POSITIONAL_ARG_BYTES,
    MAX_GIT_STASH_MESSAGE_BYTES, MAX_GIT_TAG_MESSAGE_BYTES, MAX_INCREMENTAL_STATUS_PATH_BYTES,
    MAX_INCREMENTAL_STATUS_PATHS, RebaseAction, RebaseTodo, ReflogEntry, Remote, RepoConfig,
    RepoId, RepoOperation, ResetKind, Signature, Stash, StashId, Tag, TagKind, WorkingTreeStatus,
};
pub use history::{
    MAX_QUERY_HISTORY_ERROR_BYTES, MAX_QUERY_HISTORY_SQL_BYTES, QueryHistoryPage, QueryRecord,
    QueryRecordId, QueryStatus, compact_text_preview,
};
pub use mongo::{
    InsertManyOutcome, MAX_MONGO_COLLECTION_NAME_BYTES, MAX_MONGO_DATABASE_NAME_BYTES,
    MAX_MONGO_DOCUMENT_BYTES, MAX_MONGO_FIELD_PATH_BYTES, MAX_MONGO_NESTING_DEPTH,
    MAX_MONGO_PIPELINE_STAGES, MAX_MONGO_VALUE_NODES, MongoCollection, MongoCollectionStats,
    MongoDatabase, MongoDocument, MongoIndex, MongoQueryResult, MongoQuerySpec,
    mongo_documents_retained_bytes, mongo_value_retained_bytes, validate_mongo_collection_name,
    validate_mongo_database_name, validate_mongo_document, validate_mongo_field_path,
    validate_mongo_pipeline,
};
pub use query::{
    MAX_SQL_QUERY_BYTES, Query, QueryResult, Row, Value, Warning, json_pretty_bounded,
};
pub use redis_keyspace::{
    KeyMeta, MAX_REDIS_COLLECTION_BYTES, MAX_REDIS_COLLECTION_ITEMS, MAX_REDIS_COMMAND_ARG_BYTES,
    MAX_REDIS_COMMAND_ARGS, MAX_REDIS_COMMAND_BYTES, MAX_REDIS_COMMAND_NAME_BYTES,
    MAX_REDIS_KEY_BYTES, MAX_REDIS_LOADED_ITEMS, MAX_REDIS_MATCH_PATTERN_BYTES,
    MAX_REDIS_SCAN_ALL_KEYS, MAX_REDIS_SCAN_COUNT, MAX_REDIS_VALUE_PAGE_BATCH, RedisType,
    ScanResult, validate_redis_collection_limit, validate_redis_command, validate_redis_key,
    validate_redis_match_pattern, validate_redis_scan_count,
};
pub use redis_value::{RedisValue, RedisValueLoad, RedisValuePage, StreamEntry, ValuePageCursor};
pub use resource_limits::{
    INTERACTIVE_RESULT_WARNING_BYTES, MAX_INTERACTIVE_RESULT_BYTES, MAX_METADATA_BYTES,
    MAX_METADATA_ITEMS, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS,
};
pub use schema::{Column, ColumnKind, ColumnType, ForeignKey, Index, Schema, Table};
pub use ssh::{
    MAX_CONCURRENT_TRANSFERS, MAX_QUEUED_TRANSFERS, MAX_REMOTE_ARCHIVE_DEPTH,
    MAX_REMOTE_ARCHIVE_ENTRIES, MAX_REMOTE_ARCHIVE_RETAINED_BYTES, MAX_REMOTE_DELETE_DEPTH,
    MAX_REMOTE_DELETE_ENTRIES, MAX_REMOTE_DELETE_RETAINED_BYTES, MAX_REMOTE_DIRECTORY_ENTRIES,
    MAX_REMOTE_DIRECTORY_RETAINED_BYTES, MAX_REMOTE_FILE_PREVIEW_BYTES, MAX_SSH_ENVIRONMENT_BYTES,
    MAX_SSH_FAVORITE_PATHS_PER_PROFILE, MAX_SSH_HOST_BYTES, MAX_SSH_PASSWORD_BYTES,
    MAX_SSH_PATH_BYTES, MAX_SSH_PROFILE_NAME_BYTES, MAX_SSH_PROFILES,
    MAX_SSH_TERMINALS_PER_WORKSPACE, MAX_SSH_USERNAME_BYTES, MAX_SSH_WORKSPACES,
    MAX_TRANSFER_HISTORY, OverwritePolicy, RemoteDirectory, RemoteEntry, RemoteEntryKind,
    RemoteFileChunk, RemoteFileChunkPosition, RemoteFilePreview, SshAuthMode, SshCapability,
    SshLaunchCommand, SshPathFavorites, SshProfile, SshProfileId, SshProgressFn,
    SshWorkspacePreference, SshWorkspaceState, TRANSFER_BUFFER_BYTES, TransferCancellation,
    TransferDirection, TransferId, TransferStatus, TransferTask, join_remote_path,
    parent_remote_path, validate_local_transfer_path, validate_remote_name, validate_remote_path,
};
pub use transfer::{
    ConflictPolicy, MAX_TRANSFER_WARNINGS, ProgressFn, TransferProgress, TransferSummary,
    format_bytes,
};

/// 在已小写的查询词下做不区分大小写匹配；ASCII 常见路径不分配整段小写副本。
pub fn contains_case_insensitive(text: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    // 常见的同大小写 / 无大小写文字（如中文）先走标准库子串搜索，零分配且更快。
    if text.contains(query_lower) {
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
