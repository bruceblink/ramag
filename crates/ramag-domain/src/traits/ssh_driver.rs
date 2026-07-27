//! SSH / SFTP 基础设施抽象。

use std::path::Path;

use async_trait::async_trait;

use crate::entities::{
    OverwritePolicy, RemoteDirectory, RemoteEntryKind, SshCapability, SshLaunchCommand, SshProfile,
    SshProfileId, SshProgressFn, TransferCancellation,
};
use crate::error::Result;

#[async_trait]
pub trait SshDriver: Send + Sync {
    async fn probe(&self, custom_path: Option<&str>) -> Result<SshCapability>;

    async fn terminal_command(&self, profile: &SshProfile) -> Result<SshLaunchCommand>;

    /// PTY 启动可执行文件失败后清除能力缓存，使下次连接重新发现 OpenSSH。
    async fn report_terminal_launch_failure(&self, executable: &str);

    async fn test_connection(&self, profile: &SshProfile) -> Result<()>;

    async fn list_directory(&self, profile: &SshProfile, path: &str) -> Result<RemoteDirectory>;

    async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()>;

    async fn rename(&self, profile: &SshProfile, old_path: &str, new_path: &str) -> Result<()>;

    async fn remove(&self, profile: &SshProfile, path: &str, kind: RemoteEntryKind) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn upload(
        &self,
        profile: &SshProfile,
        local_path: &Path,
        remote_path: &str,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()>;

    async fn disconnect(&self, profile_id: &SshProfileId) -> Result<()>;

    async fn shutdown(&self) -> Result<()>;
}
