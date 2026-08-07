//! 由基础设施层实现、应用层依赖的领域接口。

pub mod clipboard_driver;
pub mod doc_driver;
pub mod driver;
pub mod git_driver;
pub mod jumpserver_driver;
pub mod kv_driver;
pub mod ssh_driver;
pub mod storage;
pub mod tool;
pub mod update_driver;

pub use clipboard_driver::ClipboardDriver;
pub use doc_driver::DocDriver;
pub use driver::{CancelHandle, Driver};
pub use git_driver::GitDriver;
pub use jumpserver_driver::JumpServerDriver;
pub use kv_driver::KvDriver;
pub use ssh_driver::SshDriver;
pub use storage::Storage;
pub use tool::{Tool, ToolMeta};
pub use update_driver::UpdateDriver;
