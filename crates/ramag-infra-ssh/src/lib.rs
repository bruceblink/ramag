#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 系统 OpenSSH + 结构化 SFTP 基础设施实现。

mod askpass;
mod command;
mod diagnostic;
mod driver;
mod jumpserver;
mod runtime;
mod session;
mod support;
mod transfer;

use support::*;

pub use driver::OpenSshDriver;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::time::timeout;

use ramag_domain::entities::{
    DiagnosticCancellation, DiagnosticTermination, MAX_CONCURRENT_DIAGNOSTICS,
    MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE, MAX_PRODUCTION_DIRECTORY_ENTRIES,
    MAX_REMOTE_DIRECTORY_ENTRIES, OverwritePolicy, RemoteCapabilityState, RemoteDirectory,
    RemoteEntryKind, RemoteFileChunk, RemoteFileChunkPosition, RemoteFilePreview,
    RemoteOperatingSystem, RemotePath, RemotePlatformPreference, SftpNamespaceKind,
    SftpTransportKind, SshCapability, SshDiagnosticOperation, SshDiagnosticResult,
    SshLaunchCommand, SshProfile, SshProfileId, SshProfileOrigin, SshProgressFn,
    SshRemoteCapabilities, TransferCancellation, infer_sftp_namespace, validate_remote_path,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::SshDriver;

use crate::command::{
    OpenSshLocator, sftp_args, uses_windows_remote_sftp, windows_remote_sftp_args,
};
use crate::runtime::{run_in_tokio, tokio_runtime};
use crate::session::SessionCache;
use crate::transfer::TransferEngine;

pub use jumpserver::JumpServerHttpDriver;

const DIRECTORY_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const WINDOWS_DRIVE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

#[async_trait]
impl SshDriver for OpenSshDriver {
    async fn probe(&self, custom_path: Option<&str>) -> Result<SshCapability> {
        let locator = self.locator.clone();
        let custom_path = custom_path.map(str::to_owned);
        run_in_tokio(async move { locator.probe(custom_path).await }).await
    }

    async fn terminal_command(
        &self,
        profile: &SshProfile,
        initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        let locator = self.locator.clone();
        let askpass = self.askpass.clone();
        let profile = profile.clone();
        let initial_directory = initial_directory.map(str::to_owned);
        run_in_tokio(async move {
            let capability = locator.probe(profile.ssh_path.clone()).await?;
            let mut command =
                command::terminal_command(&profile, &capability, initial_directory.as_deref())?;
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

    async fn probe_remote_capabilities(
        &self,
        profile: &SshProfile,
    ) -> Result<SshRemoteCapabilities> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        let profile = profile.clone();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        let askpass = self.askpass.clone();
        run_in_tokio(async move {
            let mut capabilities = SshRemoteCapabilities::default();
            if let Err(error) = locator.probe(profile.ssh_path.clone()).await {
                capabilities.openssh_client = RemoteCapabilityState::Failed;
                capabilities.ssh_authentication = RemoteCapabilityState::Failed;
                capabilities.ssh_execution = RemoteCapabilityState::Failed;
                capabilities.terminal = RemoteCapabilityState::Failed;
                capabilities.sftp = RemoteCapabilityState::Failed;
                capabilities.diagnostic = RemoteCapabilityState::Failed;
                capabilities.diagnostic_message = Some(error.to_string());
                return Ok(capabilities);
            }
            capabilities.openssh_client = RemoteCapabilityState::Available;

            let platform = diagnostic::probe_operating_system(&locator, &askpass, &profile).await;
            let mut default_directory_hint = None;
            match platform {
                Ok(probe) => {
                    capabilities.operating_system = probe.operating_system;
                    capabilities.shell = probe.shell;
                    capabilities.ssh_execution = RemoteCapabilityState::Available;
                    capabilities.diagnostic = RemoteCapabilityState::Available;
                    capabilities.diagnostic_provider = Some(probe.provider);
                    default_directory_hint = probe.default_directory_hint;
                }
                Err(error) => {
                    capabilities.ssh_execution = RemoteCapabilityState::Failed;
                    capabilities.diagnostic = RemoteCapabilityState::Failed;
                    capabilities.diagnostic_message = Some(error.to_string());
                }
            }
            capabilities.terminal =
                match diagnostic::probe_interactive_terminal(&locator, &askpass, &profile).await {
                    Ok(()) => RemoteCapabilityState::Available,
                    Err(error) => {
                        capabilities
                            .diagnostic_message
                            .get_or_insert(error.to_string());
                        RemoteCapabilityState::Failed
                    }
                };

            let sftp_profile =
                profile_with_detected_platform(&profile, capabilities.operating_system);
            match connect(&locator, &sessions, &sftp_profile).await {
                Ok(connection) => {
                    let transport = connection.transport_kind();
                    capabilities.sftp_transport = Some(transport);
                    apply_sftp_transport_evidence(&mut capabilities, transport);
                    let canonical = canonicalize_sftp_initial_directory(
                        &connection.session,
                        &sftp_profile,
                        capabilities.operating_system,
                        default_directory_hint.as_deref(),
                    )
                    .await
                    .map_err(|error| session::map_sftp_error("验证 SFTP 初始目录", error));
                    match canonical {
                        Ok(canonical) => {
                            let namespace = if capabilities.operating_system
                                == RemoteOperatingSystem::Windows
                                && canonical.starts_with('/')
                            {
                                SftpNamespaceKind::Virtual
                            } else {
                                infer_sftp_namespace(&canonical)
                            };
                            match RemotePath::parse_with_namespace(&canonical, namespace) {
                                Ok(path) => {
                                    capabilities.sftp = RemoteCapabilityState::Available;
                                    capabilities.sftp_namespace = namespace;
                                    capabilities.sftp_canonical_path = Some(path);
                                }
                                Err(error) => {
                                    capabilities.sftp = RemoteCapabilityState::Unsupported;
                                    capabilities
                                        .diagnostic_message
                                        .get_or_insert(format!("SFTP 命名空间不受支持：{error}"));
                                }
                            }
                        }
                        Err(error) => {
                            capabilities.sftp = RemoteCapabilityState::Failed;
                            capabilities
                                .diagnostic_message
                                .get_or_insert(error.to_string());
                        }
                    }
                }
                Err(error) => {
                    capabilities.sftp = RemoteCapabilityState::Failed;
                    capabilities
                        .diagnostic_message
                        .get_or_insert(error.to_string());
                }
            }

            capabilities.ssh_authentication = if capabilities.ssh_execution
                == RemoteCapabilityState::Available
                || capabilities.sftp == RemoteCapabilityState::Available
                || capabilities.terminal == RemoteCapabilityState::Available
            {
                RemoteCapabilityState::Available
            } else {
                RemoteCapabilityState::Failed
            };

            let mismatch = matches!(
                (profile.remote_platform, capabilities.operating_system),
                (
                    RemotePlatformPreference::Linux,
                    RemoteOperatingSystem::Windows
                ) | (
                    RemotePlatformPreference::Windows,
                    RemoteOperatingSystem::Linux
                )
            );
            if mismatch {
                capabilities.diagnostic = RemoteCapabilityState::Failed;
                capabilities.diagnostic_provider = None;
                capabilities.diagnostic_message =
                    Some("远端平台与配置偏好冲突，请重新确认连接配置".into());
            }
            Ok(capabilities)
        })
        .await
    }

    async fn execute_diagnostic(
        &self,
        profile: &SshProfile,
        capabilities: &SshRemoteCapabilities,
        operation: &SshDiagnosticOperation,
        cancellation: DiagnosticCancellation,
    ) -> Result<SshDiagnosticResult> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        if !profile.production {
            return Err(DomainError::Forbidden(
                "安全诊断只允许生产模式连接使用".into(),
            ));
        }
        operation.validate().map_err(DomainError::InvalidConfig)?;
        if capabilities.diagnostic != RemoteCapabilityState::Available {
            return Err(DomainError::Forbidden(
                "当前连接的安全诊断能力不可用".into(),
            ));
        }
        let profile_gate = self
            .diagnostic_profiles
            .lock()
            .entry(profile.id.clone())
            .or_insert_with(|| {
                Arc::new(tokio::sync::Semaphore::new(
                    MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE,
                ))
            })
            .clone();
        let global_gate = self.diagnostic_global.clone();
        let profile = profile.clone();
        let capabilities = capabilities.clone();
        let operation = operation.clone();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        let askpass_env = self.askpass.environment(&profile)?;
        run_in_tokio(async move {
            let _global = global_gate
                .try_acquire_owned()
                .map_err(|_| DomainError::Forbidden("全局安全诊断并发已达 4 个上限".into()))?;
            let _profile = profile_gate
                .try_acquire_owned()
                .map_err(|_| DomainError::Forbidden("同一连接同时只能执行 1 个安全诊断".into()))?;
            match &operation {
                SshDiagnosticOperation::FileMetadata { path } => {
                    execute_file_metadata(&locator, &sessions, &profile, &capabilities, path).await
                }
                SshDiagnosticOperation::FileChunk { path, position } => {
                    execute_file_chunk(
                        &locator,
                        &sessions,
                        &profile,
                        &capabilities,
                        path,
                        *position,
                    )
                    .await
                }
                _ => {
                    diagnostic::execute(
                        &locator,
                        askpass_env,
                        &profile,
                        &capabilities,
                        &operation,
                        cancellation,
                    )
                    .await
                }
            }
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
            // 兼容通道完全由配置决定；标准通道只重建同类连接，不自动切换实现。
            if uses_windows_remote_sftp(&profile)
                || !matches!(first, Err(DomainError::ConnectionFailed(_)))
            {
                return first;
            }
            sessions.invalidate(&profile.id).await;
            list_once(&locator, &sessions, &profile, &path).await
        })
        .await
    }

    async fn read_file_preview(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteFilePreview> {
        validate_profile_and_path(profile, path)?;
        let profile = profile.clone();
        let path = path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection
                .contextualize(session::read_file_preview(&connection.session, &path).await);
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn read_file_chunk(
        &self,
        profile: &SshProfile,
        path: &str,
        position: RemoteFileChunkPosition,
    ) -> Result<RemoteFileChunk> {
        validate_profile_and_path(profile, path)?;
        let profile = profile.clone();
        let path = path.to_string();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection.contextualize(
                session::read_file_chunk(&connection.session, &path, position).await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn save_file(
        &self,
        profile: &SshProfile,
        path: &str,
        expected: &[u8],
        contents: &[u8],
    ) -> Result<()> {
        validate_writable_profile_and_path(profile, path)?;
        let profile = profile.clone();
        let path = path.to_string();
        let expected = expected.to_vec();
        let contents = contents.to_vec();
        let locator = self.locator.clone();
        let sessions = self.sessions.clone();
        let transfers = self.transfers.clone();
        run_in_tokio(async move {
            let connection = connect(&locator, &sessions, &profile).await?;
            let result = connection.contextualize(
                transfers
                    .save_file(connection.session.clone(), path, expected, contents)
                    .await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()> {
        validate_writable_profile_and_path(profile, path)?;
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
        validate_writable_profile_and_path(profile, old_path)?;
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
        validate_writable_profile_and_path(profile, path)?;
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
        validate_writable_profile_and_path(profile, remote_path)?;
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
                        profile.production,
                    )
                    .await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn download_directory(
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
                    .download_directory(
                        connection.session.clone(),
                        remote_path,
                        local_path,
                        overwrite,
                        cancellation,
                        progress,
                        profile.production,
                    )
                    .await,
            );
            invalidate_broken(&sessions, &profile.id, &result).await;
            result
        })
        .await
    }

    async fn disconnect(&self, profile_id: &SshProfileId) -> Result<()> {
        self.diagnostic_profiles.lock().remove(profile_id);
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

#[cfg(test)]
mod tests;
