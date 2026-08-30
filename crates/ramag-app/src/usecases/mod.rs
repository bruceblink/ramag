/// 幂等读断连时清池并重试一次；写操作不得使用。
/// 用法：`retry_idempotent_read!(config.id, self.evict_pool(config), op.await)`。
macro_rules! retry_idempotent_read {
    ($conn_id:expr, $evict:expr, $op:expr) => {{
        match $op {
            ::std::result::Result::Err(::ramag_domain::error::DomainError::ConnectionFailed(
                msg,
            )) => {
                ::tracing::warn!(
                    operation = "connection_read_retry",
                    connection_id = %$conn_id,
                    error = %msg,
                    "connection read retrying after cache eviction"
                );
                $evict;
                $op
            }
            other => other,
        }
    }};
}

pub mod clip_thumb;
pub mod clipboard_service;
pub mod connection_service;
pub mod data_sync;
pub mod export;
pub mod id_conversion;
pub mod kafka_service;
pub mod mongo_service;
pub mod object_storage_service;
pub mod redis_service;
pub mod ssh_service;
pub mod transfer;
pub mod update_service;

pub use clipboard_service::{CaptureDecision, ClipboardService, HotkeyState, decide_capture};
pub use connection_service::ConnectionService;
pub use data_sync::{
    DataSyncConfirmation, DataSyncExecutionContext, DataSyncGate, DataSyncGatePhase,
    DataSyncGateSnapshot, DataSyncObjectCatalog, DataSyncPermit, DataSyncPreflightReport,
    DataSyncService, MAX_DATA_SYNC_CATALOG_OBJECTS, PreparedDataSync, StartedDataSync,
};
pub use id_conversion::{convert_id_to_integer, convert_id_to_string};
pub use kafka_service::KafkaService;
pub use mongo_service::MongoService;
pub use object_storage_service::{
    AccountVerification, ObjectListingPage, ObjectStorageMountResult, ObjectStorageService,
    SavedObjectStorageAccount, configured_mounts,
};
pub use redis_service::RedisService;
pub use ssh_service::SshService;
pub use update_service::{
    AUTO_CHECK_INTERVAL, AvailableUpdate, UPDATE_CHECK_PREF_KEY, UpdateCheckResult, UpdatePlatform,
    UpdateService, asset_name_for, current_platform,
};
