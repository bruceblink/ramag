#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 系统 OpenSSH + 结构化 SFTP 基础设施实现。

mod askpass;
mod command;
mod diagnostic;
mod jumpserver;
mod runtime;
mod session;
mod transfer;

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
    RemoteOperatingSystem, RemotePath, RemotePlatformPreference, SftpNamespaceKind, SshCapability,
    SshDiagnosticOperation, SshDiagnosticResult, SshLaunchCommand, SshProfile, SshProfileId,
    SshProfileOrigin, SshProgressFn, SshRemoteCapabilities, TransferCancellation,
    infer_sftp_namespace, validate_remote_path,
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

pub struct OpenSshDriver {
    locator: OpenSshLocator,
    sessions: SessionCache,
    transfers: TransferEngine,
    askpass: Arc<askpass::AskPassBroker>,
    diagnostic_global: Arc<tokio::sync::Semaphore>,
    diagnostic_profiles: parking_lot::Mutex<HashMap<SshProfileId, Arc<tokio::sync::Semaphore>>>,
}

impl OpenSshDriver {
    pub fn new() -> Self {
        let askpass = Arc::new(askpass::AskPassBroker::new());
        Self {
            locator: OpenSshLocator::default(),
            sessions: SessionCache::new(askpass.clone()),
            transfers: TransferEngine::default(),
            askpass,
            diagnostic_global: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DIAGNOSTICS)),
            diagnostic_profiles: parking_lot::Mutex::new(HashMap::new()),
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

    async fn terminal_command(
        &self,
        profile: &SshProfile,
        initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        if profile.production {
            return Err(DomainError::Forbidden(
                "生产模式禁止启动完整 SSH Terminal".into(),
            ));
        }
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
                capabilities.terminal = if profile.production {
                    RemoteCapabilityState::BlockedByPolicy
                } else {
                    RemoteCapabilityState::Failed
                };
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
            capabilities.terminal = if profile.production {
                RemoteCapabilityState::BlockedByPolicy
            } else {
                match diagnostic::probe_interactive_terminal(&locator, &askpass, &profile).await {
                    Ok(()) => RemoteCapabilityState::Available,
                    Err(error) => {
                        capabilities
                            .diagnostic_message
                            .get_or_insert(error.to_string());
                        RemoteCapabilityState::Failed
                    }
                }
            };

            let sftp_profile =
                profile_with_detected_platform(&profile, capabilities.operating_system);
            match connect(&locator, &sessions, &sftp_profile).await {
                Ok(connection) => {
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

async fn connect(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
) -> Result<std::sync::Arc<session::SftpConnection>> {
    let capability = locator.probe(profile.ssh_path.clone()).await?;
    let (args, transport) = if uses_windows_remote_sftp(profile) {
        (
            windows_remote_sftp_args(profile)?,
            session::SftpTransport::WindowsRemoteServer,
        )
    } else {
        (sftp_args(profile)?, session::SftpTransport::Subsystem)
    };
    match sessions
        .get_or_connect(profile, &capability.executable, &args, transport)
        .await
    {
        Ok(connection) => Ok(connection),
        Err(error) => {
            tracing::warn!(
                profile_id = %profile.id,
                transport = ?transport,
                error = %error,
                "ssh sftp session connection failed"
            );
            locator.invalidate(&capability.executable);
            Err(error)
        }
    }
}

async fn canonicalize_sftp_initial_directory(
    session: &session::StructuredSftpSession,
    profile: &SshProfile,
    operating_system: RemoteOperatingSystem,
    default_directory_hint: Option<&str>,
) -> std::result::Result<String, russh_sftp::client::error::Error> {
    let canonical = session
        .canonicalize(profile.initial_path().to_string())
        .await?;
    let windows = operating_system == RemoteOperatingSystem::Windows
        || (operating_system == RemoteOperatingSystem::Unknown
            && profile.remote_platform == RemotePlatformPreference::Windows);
    if profile.initial_directory.is_some() || !windows || canonical != "/" {
        return Ok(canonical);
    }
    // JumpServer 的 SFTP 路径位于服务端授权根目录下，不能用终端里的 Windows
    // 绝对路径继续探测，否则 `C:/` 会被错误解释为授权根下的相对目录。
    if profile.origin == SshProfileOrigin::JumpServer {
        return Ok(canonical);
    }
    for candidate in windows_sftp_default_candidates(profile, default_directory_hint) {
        if let Ok(resolved) = session.canonicalize(candidate).await
            && resolved != "/"
        {
            return Ok(resolved);
        }
    }
    Ok(canonical)
}

fn windows_sftp_default_candidates(
    profile: &SshProfile,
    default_directory_hint: Option<&str>,
) -> Vec<String> {
    if profile.origin == SshProfileOrigin::JumpServer {
        return Vec::new();
    }
    let mut candidates = default_directory_hint
        .into_iter()
        .flat_map(windows_sftp_home_candidates)
        .collect::<Vec<_>>();
    let account = Some(profile.username.as_str());
    if let Some(account) = account.filter(|account| !account.is_empty()) {
        candidates.extend(windows_sftp_home_candidates(&format!("C:/Users/{account}")));
    }
    candidates.extend([
        "C:/Users".into(),
        "/C:/Users".into(),
        "C:/".into(),
        "/C:/".into(),
    ]);
    candidates.dedup();
    candidates
}

fn windows_sftp_home_candidates(home: &str) -> Vec<String> {
    if RemotePath::parse_server_canonical(home).is_err() {
        return Vec::new();
    }
    let mut candidates = vec![home.to_string()];
    if infer_sftp_namespace(home) == SftpNamespaceKind::WindowsDrive {
        candidates.push(format!("/{home}"));
    }
    candidates
}

async fn list_once(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    path: &str,
) -> Result<RemoteDirectory> {
    let connection = connect(locator, sessions, profile).await?;
    let max_entries = if profile.production {
        MAX_PRODUCTION_DIRECTORY_ENTRIES
    } else {
        MAX_REMOTE_DIRECTORY_ENTRIES
    };
    let result = timeout(DIRECTORY_REQUEST_TIMEOUT, async {
        let mut directory = connection
            .contextualize(session::list_directory(&connection.session, path, max_entries).await)?;
        if should_list_windows_drives(profile, path) {
            match timeout(
                WINDOWS_DRIVE_DISCOVERY_TIMEOUT,
                session::list_windows_drives(&connection.session),
            )
            .await
            {
                Ok(drives) if !drives.is_empty() => {
                    directory.path = "/".into();
                    directory.entries = drives;
                }
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        profile_id = %profile.id,
                        "windows sftp drive discovery timed out"
                    );
                }
            }
        }
        Ok(directory)
    })
    .await;
    match result {
        Ok(directory) => directory,
        Err(_) => {
            tracing::warn!(
                profile_id = %profile.id,
                path,
                timeout_seconds = DIRECTORY_REQUEST_TIMEOUT.as_secs(),
                "ssh sftp directory request timed out"
            );
            sessions.invalidate(&profile.id).await;
            Err(DomainError::ConnectionFailed(format!(
                "读取远程目录超时（{} 秒）",
                DIRECTORY_REQUEST_TIMEOUT.as_secs()
            )))
        }
    }
}

fn should_list_windows_drives(profile: &SshProfile, requested_path: &str) -> bool {
    profile.remote_platform == RemotePlatformPreference::Windows
        && (profile.origin != SshProfileOrigin::JumpServer || uses_windows_remote_sftp(profile))
        && matches!(requested_path, "." | "/")
}

fn profile_with_detected_platform(
    profile: &SshProfile,
    operating_system: RemoteOperatingSystem,
) -> SshProfile {
    let mut effective = profile.clone();
    if effective.remote_platform == RemotePlatformPreference::Auto {
        effective.remote_platform = match operating_system {
            RemoteOperatingSystem::Linux => RemotePlatformPreference::Linux,
            RemoteOperatingSystem::Windows => RemotePlatformPreference::Windows,
            RemoteOperatingSystem::Unknown => RemotePlatformPreference::Auto,
        };
    }
    effective
}

async fn execute_file_metadata(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    path: &RemotePath,
) -> Result<SshDiagnosticResult> {
    validate_diagnostic_path(capabilities, path)?;
    let started = Instant::now();
    let connection = connect(locator, sessions, profile).await?;
    let metadata = connection
        .session
        .raw
        .lstat(path.canonical().to_string())
        .await
        .map_err(|error| session::map_sftp_error("读取远程文件元信息", error))?;
    let attrs = metadata.attrs;
    let kind = if attrs.is_regular() {
        "file"
    } else if attrs.is_dir() {
        "directory"
    } else if attrs.is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let output = serde_json::to_string(&serde_json::json!({
        "path": path.canonical(),
        "kind": kind,
        "size": attrs.size,
        "permissions": attrs.permissions,
        "modifiedAtUnix": attrs.mtime,
    }))
    .map_err(|error| DomainError::Other(format!("序列化远程文件元信息失败：{error}")))?;
    diagnostic_result(profile, capabilities, "file_metadata", output, started)
}

async fn execute_file_chunk(
    locator: &OpenSshLocator,
    sessions: &SessionCache,
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    path: &RemotePath,
    position: RemoteFileChunkPosition,
) -> Result<SshDiagnosticResult> {
    validate_diagnostic_path(capabilities, path)?;
    let started = Instant::now();
    let connection = connect(locator, sessions, profile).await?;
    let chunk = session::read_file_chunk(&connection.session, path.canonical(), position).await?;
    let text = String::from_utf8(chunk.bytes)
        .map_err(|_| DomainError::InvalidConfig("安全诊断文件片段只支持有效 UTF-8 文本".into()))?;
    let output = sanitize_diagnostic_text(&text);
    diagnostic_result(profile, capabilities, "file_chunk", output, started)
}

fn validate_diagnostic_path(capabilities: &SshRemoteCapabilities, path: &RemotePath) -> Result<()> {
    if capabilities.sftp != RemoteCapabilityState::Available {
        return Err(DomainError::Forbidden("当前连接的 SFTP 能力不可用".into()));
    }
    if path.namespace() != capabilities.sftp_namespace {
        return Err(DomainError::InvalidConfig(
            "诊断路径不属于当前 SFTP 命名空间".into(),
        ));
    }
    Ok(())
}

fn diagnostic_result(
    profile: &SshProfile,
    capabilities: &SshRemoteCapabilities,
    operation: &str,
    output: String,
    started: Instant,
) -> Result<SshDiagnosticResult> {
    let provider = capabilities
        .diagnostic_provider
        .ok_or_else(|| DomainError::Forbidden("当前连接没有可用的安全诊断提供者".into()))?;
    Ok(SshDiagnosticResult {
        profile_id: profile.id.clone(),
        operation: operation.into(),
        operating_system: capabilities.operating_system,
        provider,
        output,
        exit_code: Some(0),
        termination: DiagnosticTermination::Completed,
        truncated: false,
        elapsed_millis: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

fn sanitize_diagnostic_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| matches!(character, '\n' | '\r' | '\t') || !character.is_control())
        .collect()
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

fn validate_writable_profile_and_path(profile: &SshProfile, path: &str) -> Result<()> {
    validate_profile_and_path(profile, path)?;
    if profile.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_driver_can_be_created_without_runtime_side_effects() {
        let _driver = OpenSshDriver::default();
    }

    #[test]
    fn production_profile_is_rejected_before_infra_write_connection() {
        let mut profile = SshProfile::new("production", "server.example");
        profile.production = true;

        assert!(matches!(
            validate_writable_profile_and_path(&profile, "/remote/file"),
            Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
        ));
        assert!(validate_profile_and_path(&profile, "/remote/file").is_ok());
    }

    #[test]
    fn windows_sftp_home_tries_native_and_virtual_drive_forms() {
        assert_eq!(
            windows_sftp_home_candidates("C:/Users/Administrator"),
            ["C:/Users/Administrator", "/C:/Users/Administrator"]
        );
        assert!(windows_sftp_home_candidates("relative/path").is_empty());
    }

    #[test]
    fn jumpserver_windows_default_does_not_escape_the_authorized_root() {
        let mut profile = SshProfile::new("windows", "jump.example.com");
        profile.origin = ramag_domain::entities::SshProfileOrigin::JumpServer;
        profile.username = "alice#administrator#asset-id".into();

        let candidates = windows_sftp_default_candidates(&profile, None);

        assert!(candidates.is_empty());
    }

    #[test]
    fn windows_drive_discovery_runs_for_every_accessible_windows_transport_root() {
        let mut windows = SshProfile::new("windows", "windows.example");
        windows.remote_platform = RemotePlatformPreference::Windows;

        assert!(should_list_windows_drives(&windows, "."));
        assert!(should_list_windows_drives(&windows, "/"));
        assert!(!should_list_windows_drives(
            &windows,
            "C:/Users/Administrator"
        ));

        let linux = SshProfile::new("linux", "linux.example");
        assert!(!should_list_windows_drives(&linux, "/"));

        let mut jumpserver = SshProfile::new("jumpserver", "jump.example");
        jumpserver.origin = SshProfileOrigin::JumpServer;
        assert!(!should_list_windows_drives(&jumpserver, "/"));
        jumpserver.remote_platform = RemotePlatformPreference::Windows;
        assert!(should_list_windows_drives(&jumpserver, "/"));
        jumpserver.production = true;
        assert!(!should_list_windows_drives(&jumpserver, "/"));
    }

    #[test]
    fn auto_profile_uses_the_detected_platform_for_sftp_only() {
        let mut profile = SshProfile::new("jumpserver", "jump.example.com");
        profile.remote_platform = RemotePlatformPreference::Auto;
        profile.origin = SshProfileOrigin::JumpServer;

        let effective = profile_with_detected_platform(&profile, RemoteOperatingSystem::Windows);

        assert_eq!(effective.remote_platform, RemotePlatformPreference::Windows);
        assert_eq!(effective.id, profile.id);
        assert_eq!(effective.username, profile.username);
        assert_eq!(profile.remote_platform, RemotePlatformPreference::Auto);
    }
}
