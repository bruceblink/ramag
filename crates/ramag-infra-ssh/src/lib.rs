#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 系统 OpenSSH + 结构化 SFTP 基础设施实现。

mod askpass;
mod command;
mod runtime;
mod session;
mod transfer;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use ramag_domain::entities::{
    OverwritePolicy, RemoteDirectory, RemoteEntryKind, SshCapability, SshLaunchCommand, SshProfile,
    SshProfileId, SshProgressFn, TransferCancellation, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::SshDriver;

use crate::command::{OpenSshLocator, sftp_args};
use crate::runtime::{run_in_tokio, tokio_runtime};
use crate::session::SessionCache;
use crate::transfer::TransferEngine;

pub struct OpenSshDriver {
    locator: OpenSshLocator,
    sessions: SessionCache,
    transfers: TransferEngine,
    askpass: Arc<askpass::AskPassBroker>,
}

impl OpenSshDriver {
    pub fn new() -> Self {
        let askpass = Arc::new(askpass::AskPassBroker::new());
        Self {
            locator: OpenSshLocator::default(),
            sessions: SessionCache::new(askpass.clone()),
            transfers: TransferEngine::default(),
            askpass,
        }
    }
}

impl Default for OpenSshDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OpenSshDriver {
    fn drop(&mut self) {
        let sessions = self.sessions.clone();
        if let Ok(runtime) = tokio_runtime() {
            runtime.spawn(async move {
                sessions.shutdown().await;
            });
        }
    }
}

#[async_trait]
impl SshDriver for OpenSshDriver {
    async fn probe(&self, custom_path: Option<&str>) -> Result<SshCapability> {
        let locator = self.locator.clone();
        let custom_path = custom_path.map(str::to_owned);
        run_in_tokio(async move { locator.probe(custom_path).await }).await
    }

    async fn terminal_command(&self, profile: &SshProfile) -> Result<SshLaunchCommand> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        let locator = self.locator.clone();
        let askpass = self.askpass.clone();
        let profile = profile.clone();
        run_in_tokio(async move {
            let capability = locator.probe(profile.ssh_path.clone()).await?;
            let mut command = command::terminal_command(&profile, &capability)?;
            command.env = askpass.environment(&profile)?;
            Ok(command)
        })
        .await
    }

    async fn report_terminal_launch_failure(&self, executable: &str) {
        self.locator.invalidate(executable);
    }

    async fn test_connection(&self, profile: &SshProfile) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        // 连接测试使用独立缓存键，并在返回前关闭，避免干扰已打开工作区或累积草稿会话。
        let mut profile = profile.clone();
        profile.id = SshProfileId::new();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection
                .session
                .canonicalize(profile.initial_path().to_string())
                .await
                .map(|_| ())
                .map_err(|error| session::map_sftp_error("验证 SFTP 初始目录", error));
            let result = connection.contextualize(result);
            sessions.invalidate(&profile.id).await;
            result
        })
        .await
    }

    async fn list_directory(&self, profile: &SshProfile, path: &str) -> Result<RemoteDirectory> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        let profile = profile.clone();
        let path = path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let first = list_once(&locator, &sessions, &profile, &path).await;
            if !matches!(first, Err(DomainError::ConnectionFailed(_))) {
                return first;
            }
            sessions.invalidate(&profile.id).await;
            list_once(&locator, &sessions, &profile, &path).await
        })
        .await
    }

    async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()> {
        validate_profile_and_path(profile, path)?;
        let profile = profile.clone();
        let path = path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection
                .contextualize(session::create_directory(&connection.session, &path).await);
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn rename(&self, profile: &SshProfile, old_path: &str, new_path: &str) -> Result<()> {
        validate_profile_and_path(profile, old_path)?;
        validate_remote_path(new_path).map_err(DomainError::InvalidConfig)?;
        let profile = profile.clone();
        let old_path = old_path.to_string();
        let new_path = new_path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection
                .contextualize(session::rename(&connection.session, &old_path, &new_path).await);
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn remove(&self, profile: &SshProfile, path: &str, kind: RemoteEntryKind) -> Result<()> {
        validate_profile_and_path(profile, path)?;
        let profile = profile.clone();
        let path = path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result =
                connection.contextualize(session::remove(&connection.session, &path, kind).await);
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn upload(
        &self,
        profile: &SshProfile,
        local_path: &Path,
        remote_path: &str,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()> {
        validate_profile_and_path(profile, remote_path)?;
        let profile = profile.clone();
        let local_path = local_path.to_path_buf();
        let remote_path = remote_path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        let transfers = self.transfers.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection.contextualize(
                transfers
                    .upload(
                        connection.session.clone(),
                        local_path,
                        remote_path,
                        overwrite,
                        cancellation,
                        progress,
                    )
                    .await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()> {
        validate_profile_and_path(profile, remote_path)?;
        let profile = profile.clone();
        let remote_path = remote_path.to_string();
        let local_path = local_path.to_path_buf();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        let transfers = self.transfers.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection.contextualize(
                transfers
                    .download(
                        connection.session.clone(),
                        remote_path,
                        local_path,
                        overwrite,
                        cancellation,
                        progress,
                    )
                    .await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn disconnect(&self, profile_id: &SshProfileId) -> Result<()> {
        let profile_id = profile_id.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            sessions.invalidate(&profile_id).await;
            Ok(())
        })
        .await
    }

    async fn shutdown(&self) -> Result<()> {
        let sessions = self.sessions.clone();
        self.askpass.clear();
        run_in_tokio(async move {
            sessions.shutdown().await;
            Ok(())
        })
        .await
    }
}

/// 在主程序初始化前处理 OpenSSH AskPass 子进程请求。
pub fn run_askpass_helper(confirm: impl FnOnce(&str) -> bool) -> Option<i32> {
    askpass::run_helper(confirm)
}

async fn connect(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
) -> Result<std::sync::Arc<session::SftpConnection>> {
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let args = sftp_args(profile)?;
    match sessions
        .get_or_connect(profile, &capability.executable, &args)
        .await
    {
        Ok(connection) => Ok(connection),
        Err(error) => {
            locator.invalidate(&capability.executable);
            Err(error)
        }
    }
}

async fn list_once(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    path: &str,
) -> Result<RemoteDirectory> {
    let connection = connect(locator, sessions, profile).await?;
    connection.contextualize(session::list_directory(&connection.session, path).await)
}

async fn invalidate_broken<T>(
    sessions: &SessionCache,
    profile_id: &SshProfileId,
    result: &Result<T>,
) {
    if matches!(result, Err(DomainError::ConnectionFailed(_))) {
        sessions.invalidate(profile_id).await;
    }
}

fn validate_profile_and_path(profile: &SshProfile, path: &str) -> Result<()> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    validate_remote_path(path).map_err(DomainError::InvalidConfig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_driver_can_be_created_without_runtime_side_effects() {
        let _driver = OpenSshDriver::default();
    }
}
