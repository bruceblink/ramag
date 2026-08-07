// 测试允许 unwrap、expect 和 panic，不影响生产代码审计。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 应用层：编排领域接口实现业务用例。

mod blocking;
pub mod connection_transfer;
pub mod tool_registry;
pub mod usecases;

pub use blocking::run_blocking;
pub use tool_registry::ToolRegistry;
pub use usecases::{
    AUTO_CHECK_INTERVAL, AvailableUpdate, ClipboardService, ConnectionService,
    DataSyncConfirmation, DataSyncExecutionContext, DataSyncGate, DataSyncGatePhase,
    DataSyncGateSnapshot, DataSyncObjectCatalog, DataSyncPermit, DataSyncPreflightReport,
    DataSyncService, HotkeyState, MAX_DATA_SYNC_CATALOG_OBJECTS, MongoService, PreparedDataSync,
    RedisService, SshService, StartedDataSync, UPDATE_CHECK_PREF_KEY, UpdateCheckResult,
    UpdatePlatform, UpdateService, asset_name_for, convert_id_to_integer, convert_id_to_string,
    current_platform,
};
