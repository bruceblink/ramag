//! SSH / SFTP 基础设施抽象。

use std::path::Path;

use async_trait::async_trait;

use crate::entities::{
    OverwritePolicy, RemoteDirectory, RemoteEntryKind, RemoteFileChunk, RemoteFileChunkPosition,
    RemoteFilePreview, SshCapability, SshLaunchCommand, SshProfile, SshProfileId, SshProgressFn,
    TransferCancellation,
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

    async fn read_file_preview(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteFilePreview>;

    /// 按位置读取一个有界片段，用于查看任意大小的远程文本文件。
    async fn read_file_chunk(
        &self,
        profile: &SshProfile,
        path: &str,
        position: RemoteFileChunkPosition,
    ) -> Result<RemoteFileChunk>;

    /// 以打开编辑器时读取到的内容作为并发校验，通过临时文件安全替换远程普通文件。
    async fn save_file(
        &self,
        profile: &SshProfile,
        path: &str,
        expected: &[u8],
        contents: &[u8],
    ) -> Result<()>;

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

    #[allow(clippy::too_many_arguments)]
    async fn download_directory(
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
