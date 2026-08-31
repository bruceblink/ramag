pub mod clipboard;
pub mod connection;
pub mod data_sync;
pub mod ddl;
pub mod git;
pub mod history;
pub mod id_conversion;
pub mod jumpserver;
pub mod kafka;
#[cfg(test)]
mod kafka_tests;
pub mod mongo;
pub mod object_storage;
pub mod query;
pub mod redis_keyspace;
pub mod redis_value;
pub mod resource_limits;
pub mod schema;
pub mod ssh;
pub mod ssh_diagnostic;
pub mod ssh_remote_path;
pub mod transaction;
pub mod transfer;
pub mod update;

pub use clipboard::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSearchResult, ClipSource, ClipboardSettings,
    MAX_CLIPBOARD_ITEM_BYTES, MAX_CLIPBOARD_SEARCH_BYTES, classify_text, fnv1a_hash,
    is_safe_http_url, make_preview, parse_hex_color,
};
pub use connection::{
    ConnectionConfig, ConnectionId, DriverKind, MAX_CONNECTION_CONFIGS,
    MAX_CONNECTION_ENVIRONMENT_BYTES, MAX_CONNECTION_HOST_BYTES, MAX_CONNECTION_IDENTIFIER_BYTES,
    MAX_CONNECTION_NAME_BYTES, MAX_CONNECTION_PASSWORD_BYTES, MAX_CONNECTION_PATH_BYTES,
    MAX_CONNECTION_REMARK_BYTES, MAX_CONNECTION_SSH_TARGET_BYTES, TlsVerify,
};
pub use data_sync::{
    DataSyncProgress, DataSyncRequest, DataSyncScope, DataSyncStage, DataSyncSummary,
    DataSyncTaskId, MAX_MYSQL_SYNC_IDENTIFIER_CHARS, MAX_POSTGRES_SYNC_IDENTIFIER_BYTES,
    MongoSyncScope, SqlIdentityKind, SqlRecordIdentity, SqlSyncScope, SyncObjectMapping,
    SyncObjectSelection, SyncObjectState, SyncPlannedObject, SyncTargetFingerprint,
    select_sql_record_identity,
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
pub use id_conversion::{
    BASE10_ALPHABET, BASE16_ALPHABET, BASE36_ALPHABET, BASE58_BITCOIN_ALPHABET,
    BASE58_FLICKR_ALPHABET, IdConverterConfig, IdConverterKind, MAX_CUSTOM_ID_ALPHABET_BYTES,
    MAX_ID_CONVERTER_PROGRAM_BYTES, parse_nonnegative_id_integer, validate_custom_alphabet,
};
pub use jumpserver::{
    JumpServerAccount, JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog,
    JumpServerConnection, JumpServerCredential, JumpServerLabel, JumpServerNode,
    JumpServerOrganization, JumpServerRdpSession, JumpServerRdpSessionHistory, JumpServerSession,
    MAX_JUMPSERVER_ASSETS, MAX_JUMPSERVER_NODES, MAX_JUMPSERVER_RDP_FAVORITE_SESSIONS,
    MAX_JUMPSERVER_RDP_RECENT_SESSIONS, MAX_JUMPSERVER_TOKEN_BYTES, MAX_JUMPSERVER_URL_BYTES,
};
pub use kafka::{
    DEFAULT_KAFKA_MAX_BYTES, DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS, DEFAULT_KAFKA_MAX_RECORDS,
    DEFAULT_KAFKA_MAX_SCAN_SECONDS, KafkaAcl, KafkaAclOperation, KafkaAclPatternType,
    KafkaAclPermission, KafkaAclResourceType, KafkaBroker, KafkaClusterConfig, KafkaClusterId,
    KafkaClusterMetadata, KafkaConfigEntry, KafkaConfigResource, KafkaConfigResourceType,
    KafkaConfigSource, KafkaConfigUpdateOperation, KafkaConfigUpdateRequest, KafkaConsumerGroup,
    KafkaConsumerGroupOffset, KafkaConsumerMember, KafkaConsumerPartitionAssignment,
    KafkaMessageHeader, KafkaMessagePage, KafkaMessageQuery, KafkaMessageRecord,
    KafkaMessageSearchField, KafkaMessageSearchQuery, KafkaPartition, KafkaReadOnlyState,
    KafkaSaslMechanism, KafkaSecurityProtocol, KafkaTextPreview, KafkaTlsConfig, KafkaTopic,
    KafkaTopicCreateRequest, KafkaTopicPartitionExpansion, MAX_KAFKA_ACL_HOST_BYTES,
    MAX_KAFKA_ACL_RESOURCE_NAME_BYTES, MAX_KAFKA_ACLS, MAX_KAFKA_BOOTSTRAP_SERVER_BYTES,
    MAX_KAFKA_BOOTSTRAP_SERVERS, MAX_KAFKA_BOOTSTRAP_SERVERS_BYTES, MAX_KAFKA_BROKERS,
    MAX_KAFKA_CLIENT_ID_BYTES, MAX_KAFKA_CLUSTER_ID_BYTES, MAX_KAFKA_CLUSTER_NAME_BYTES,
    MAX_KAFKA_CLUSTERS, MAX_KAFKA_CONCURRENT_PARTITIONS, MAX_KAFKA_CONFIG_ENTRIES,
    MAX_KAFKA_CONFIG_KEY_BYTES, MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES, MAX_KAFKA_CONFIG_VALUE_BYTES,
    MAX_KAFKA_CONSUMER_GROUPS, MAX_KAFKA_GROUP_MEMBERS, MAX_KAFKA_GROUP_OFFSETS,
    MAX_KAFKA_HEADER_KEY_BYTES, MAX_KAFKA_MESSAGE_HEADERS, MAX_KAFKA_MESSAGE_PREVIEW_BYTES,
    MAX_KAFKA_PARTITIONS, MAX_KAFKA_PRINCIPAL_BYTES, MAX_KAFKA_QUERY_PARTITIONS,
    MAX_KAFKA_QUERY_TEXT_BYTES, MAX_KAFKA_REMARK_BYTES, MAX_KAFKA_REPLICAS,
    MAX_KAFKA_SASL_PASSWORD_BYTES, MAX_KAFKA_SASL_USERNAME_BYTES, MAX_KAFKA_SCAN_BYTES,
    MAX_KAFKA_SCAN_RECORDS, MAX_KAFKA_SCAN_SECONDS, MAX_KAFKA_TLS_PATH_BYTES,
    MAX_KAFKA_TOPIC_NAME_BYTES, MAX_KAFKA_TOPICS, MAX_KAFKA_VERSION_BYTES, preview_bytes,
    validate_kafka_bootstrap_server, validate_kafka_managed_topic_name, validate_kafka_topic_name,
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
pub use object_storage::{
    CloudProvider, HttpsEndpoint, MAX_MANUAL_BUCKETS_PER_ACCOUNT,
    MAX_OBJECT_STORAGE_ACCESS_KEY_ID_BYTES, MAX_OBJECT_STORAGE_ACCESS_KEY_SECRET_BYTES,
    MAX_OBJECT_STORAGE_ACCOUNT_NAME_BYTES, MAX_OBJECT_STORAGE_ACCOUNTS,
    MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES, MAX_OBJECT_STORAGE_CONCURRENT_TRANSFERS,
    MAX_OBJECT_STORAGE_ENDPOINT_BYTES, MAX_OBJECT_STORAGE_KEY_BYTES,
    MAX_OBJECT_STORAGE_PAGE_ENTRIES, MAX_OBJECT_STORAGE_QUEUED_TRANSFERS,
    MAX_OBJECT_STORAGE_REGION_BYTES, MAX_OBJECT_STORAGE_SESSION_BYTES,
    MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES, MAX_OBJECT_STORAGE_TRANSFER_HISTORY,
    MAX_OBJECT_STORAGE_WORKSPACE_BYTES, MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES, ManualBucket,
    OBJECT_STORAGE_ACCOUNT_SCHEMA_VERSION, OBJECT_STORAGE_TRANSFER_BUFFER_BYTES,
    ObjectCapabilities, ObjectDownloadRequest, ObjectEntry, ObjectEntryKind, ObjectListCursor,
    ObjectListQuery, ObjectMetadata, ObjectPage, ObjectProgressFn, ObjectStorageAccount,
    ObjectStorageAccountId, ObjectStorageAccountSnapshot, ObjectStorageFavorite,
    ObjectStorageMount, ObjectStorageMountId, ObjectStorageSessionPreference,
    ObjectStorageWorkspacePreference, ObjectStorageWorkspaceState, ObjectTextPreview,
    ObjectTransferProgress, ObjectUploadRequest, SecretString, is_opendal_safe_key,
    is_opendal_safe_list_prefix, is_opendal_safe_prefix, validate_bucket_name,
    validate_bucket_name_for_provider, validate_object_key, validate_object_name_prefix,
    validate_prefix, validate_region, validate_root_prefix,
};
pub use query::{
    MAX_SQL_QUERY_BYTES, Query, QueryResult, Row, Value, Warning, json_pretty_bounded,
};
pub use redis_keyspace::{
    KeyMeta, MAX_REDIS_COLLECTION_BYTES, MAX_REDIS_COLLECTION_ITEMS, MAX_REDIS_COMMAND_ARG_BYTES,
    MAX_REDIS_COMMAND_ARGS, MAX_REDIS_COMMAND_BYTES, MAX_REDIS_COMMAND_NAME_BYTES,
    MAX_REDIS_KEY_BYTES, MAX_REDIS_KEY_TYPE_BATCH, MAX_REDIS_LOADED_ITEMS,
    MAX_REDIS_MATCH_PATTERN_BYTES, MAX_REDIS_SCAN_ALL_KEYS, MAX_REDIS_SCAN_COUNT,
    MAX_REDIS_VALUE_PAGE_BATCH, RedisType, ScanResult, validate_redis_collection_limit,
    validate_redis_command, validate_redis_key, validate_redis_match_pattern,
    validate_redis_scan_count,
};
pub use redis_value::{RedisValue, RedisValueLoad, RedisValuePage, StreamEntry, ValuePageCursor};
pub use resource_limits::{
    INTERACTIVE_RESULT_WARNING_BYTES, MAX_INTERACTIVE_RESULT_BYTES, MAX_METADATA_BYTES,
    MAX_METADATA_ITEMS, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS,
};
pub use schema::{
    Column, ColumnKind, ColumnType, ForeignKey, ForeignKeyAction, GeneratedColumnStorage,
    IdentityGeneration, Index, Schema, Table, Trigger,
};
pub use ssh::{
    MAX_CONCURRENT_PRODUCTION_DOWNLOADS, MAX_CONCURRENT_TRANSFERS,
    MAX_PRODUCTION_DIRECTORY_ENTRIES, MAX_PRODUCTION_DOWNLOAD_BYTES,
    MAX_PRODUCTION_DOWNLOAD_SECONDS, MAX_QUEUED_TRANSFERS, MAX_REMOTE_ARCHIVE_DEPTH,
    MAX_REMOTE_ARCHIVE_ENTRIES, MAX_REMOTE_ARCHIVE_RETAINED_BYTES, MAX_REMOTE_DELETE_DEPTH,
    MAX_REMOTE_DELETE_ENTRIES, MAX_REMOTE_DELETE_RETAINED_BYTES, MAX_REMOTE_DIRECTORY_ENTRIES,
    MAX_REMOTE_DIRECTORY_RETAINED_BYTES, MAX_REMOTE_FILE_PREVIEW_BYTES, MAX_SSH_ENVIRONMENT_BYTES,
    MAX_SSH_FAVORITE_PATHS_PER_PROFILE, MAX_SSH_HOST_BYTES, MAX_SSH_PASSWORD_BYTES,
    MAX_SSH_PATH_BYTES, MAX_SSH_PROFILE_NAME_BYTES, MAX_SSH_PROFILES,
    MAX_SSH_TERMINALS_PER_WORKSPACE, MAX_SSH_USERNAME_BYTES, MAX_SSH_WORKSPACES,
    MAX_TRANSFER_HISTORY, OverwritePolicy, RemoteDirectory, RemoteEntry, RemoteEntryKind,
    RemoteFileChunk, RemoteFileChunkPosition, RemoteFilePreview, SshAuthMode, SshCapability,
    SshLaunchCommand, SshModuleSettings, SshPathFavorites, SshProfile, SshProfileId,
    SshProfileOrigin, SshProgressFn, SshWorkspacePreference, SshWorkspaceState,
    TRANSFER_BUFFER_BYTES, TransferCancellation, TransferDirection, TransferId, TransferStatus,
    TransferTask, join_remote_path, parent_remote_path, validate_local_transfer_path,
    validate_remote_name, validate_remote_path,
};
pub use ssh_diagnostic::{
    DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS, DiagnosticCancellation, DiagnosticErrorCode,
    DiagnosticOperationClass, DiagnosticTermination, DiagnosticTimeRange,
    MAX_CONCURRENT_DIAGNOSTICS, MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE, MAX_DIAGNOSTIC_INPUT_BYTES,
    MAX_DIAGNOSTIC_ITEMS, MAX_DIAGNOSTIC_OUTPUT_BYTES, MAX_DIAGNOSTIC_STDERR_BYTES,
    MAX_DIAGNOSTIC_TIMEOUT_SECONDS, MIN_DIAGNOSTIC_REFRESH_SECONDS, RemoteCapabilityState,
    RemoteOperatingSystem, RemotePlatformPreference, RemoteShellKind, SftpTransportKind,
    SshDiagnosticOperation, SshDiagnosticProviderKind, SshDiagnosticResult, SshLogSource,
    SshRemoteCapabilities, SshServiceName,
};
pub use ssh_remote_path::{
    RemotePath, SftpNamespaceKind, infer_sftp_namespace, validate_remote_name_for_namespace,
};
pub use transaction::TransactionId;
pub use transfer::{
    ConflictPolicy, MAX_TRANSFER_WARNINGS, ProgressFn, TransferProgress, TransferSummary,
    format_bytes,
};
pub use update::{
    DownloadProgress, ReleaseAsset, ReleaseInfo, UpdateCancellation, UpdateProgressFn,
};

/// 使用已转为小写的查询词匹配，ASCII 路径不分配正文副本。
pub fn contains_case_insensitive(text: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    // 中文等无大小写文本可直接命中。
    if text.contains(query_lower) {
        return true;
    }
    // ASCII 不会命中 UTF-8 多字节中的高位字节，可安全逐字节比较。
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
