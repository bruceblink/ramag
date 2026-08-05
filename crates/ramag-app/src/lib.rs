// 测试场景放开 unwrap/expect/panic（断言失败即阻断），不影响生产代码审计
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 应用层：Use Cases + ToolRegistry。依赖 domain trait，不持具体实现

mod blocking;
pub mod connection_transfer;
pub mod tool_registry;
pub mod usecases;

pub use blocking::run_blocking;
pub use tool_registry::ToolRegistry;
pub use usecases::{
    ClipboardService, ConnectionService, DataSyncConfirmation, DataSyncExecutionContext,
    DataSyncGate, DataSyncGatePhase, DataSyncGateSnapshot, DataSyncObjectCatalog, DataSyncPermit,
    DataSyncPreflightReport, DataSyncService, HotkeyState, MAX_DATA_SYNC_CATALOG_OBJECTS,
    MongoService, PreparedDataSync, RedisService, SshService, StartedDataSync,
    convert_id_to_integer, convert_id_to_string,
};
