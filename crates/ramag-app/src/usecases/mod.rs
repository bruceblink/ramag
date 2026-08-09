//! 业务用例编排。

/// 连接失败时清除缓存并重试一次幂等读操作。
/// 必须用宏而非 async 闭包助手：闭包捕获 &self / &config 后，在 `background_spawn` 等 Send
/// 上下文会触发「Send is not general enough」（async 闭包 + HRTB 的编译器限制）；宏直接把
/// 操作内联进 async fn，无此问题。
/// 仅限幂等读；写操作重试可能导致重复执行。
/// 用法：`retry_idempotent_read!(config.id, self.evict_pool(config), self.driver.xxx(..).await)`
/// 定义在子模块之前，使各服务可按 `macro_rules!` 文本作用域直接调用。
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
pub mod mongo_service;
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
pub use mongo_service::MongoService;
pub use redis_service::RedisService;
pub use ssh_service::SshService;
pub use update_service::{
    AUTO_CHECK_INTERVAL, AvailableUpdate, UPDATE_CHECK_PREF_KEY, UpdateCheckResult, UpdatePlatform,
    UpdateService, asset_name_for, current_platform,
};
