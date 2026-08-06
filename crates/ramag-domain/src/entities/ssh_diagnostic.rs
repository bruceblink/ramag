//! SSH 远端能力与生产低影响只读诊断领域模型。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::ssh::{RemoteFileChunkPosition, SshProfileId};
use super::ssh_remote_path::{RemotePath, SftpNamespaceKind};

pub const MAX_CONCURRENT_DIAGNOSTICS: usize = 4;
pub const MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE: usize = 1;
pub const MAX_DIAGNOSTIC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_STDERR_BYTES: usize = 64 * 1024;
pub const MAX_DIAGNOSTIC_ITEMS: usize = 5_000;
pub const MAX_DIAGNOSTIC_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_DIAGNOSTIC_TIMEOUT_SECONDS: u64 = 30;
pub const DEFAULT_DIAGNOSTIC_TIMEOUT_SECONDS: u64 = 10;
pub const MIN_DIAGNOSTIC_REFRESH_SECONDS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemotePlatformPreference {
    #[default]
    Auto,
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperatingSystem {
    Linux,
    Windows,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteShellKind {
    Posix,
    Cmd,
    WindowsPowerShell,
    PowerShellCore,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshDiagnosticProviderKind {
    LinuxBuiltinV1,
    WindowsPowerShellV1,
    GatewayV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCapabilityState {
    Available,
    Unsupported,
    Failed,
    BlockedByPolicy,
    #[default]
    NotProbed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SftpTransportKind {
    StandardSubsystem,
    WindowsCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshRemoteCapabilities {
    pub openssh_client: RemoteCapabilityState,
    pub ssh_authentication: RemoteCapabilityState,
    pub operating_system: RemoteOperatingSystem,
    pub shell: RemoteShellKind,
    pub ssh_execution: RemoteCapabilityState,
    pub terminal: RemoteCapabilityState,
    pub sftp: RemoteCapabilityState,
    pub sftp_namespace: SftpNamespaceKind,
    pub sftp_canonical_path: Option<RemotePath>,
    #[serde(default)]
    pub sftp_transport: Option<SftpTransportKind>,
    pub diagnostic: RemoteCapabilityState,
    pub diagnostic_provider: Option<SshDiagnosticProviderKind>,
    pub diagnostic_message: Option<String>,
}

impl Default for SshRemoteCapabilities {
    fn default() -> Self {
        Self {
            openssh_client: RemoteCapabilityState::NotProbed,
            ssh_authentication: RemoteCapabilityState::NotProbed,
            operating_system: RemoteOperatingSystem::Unknown,
            shell: RemoteShellKind::Unknown,
            ssh_execution: RemoteCapabilityState::NotProbed,
            terminal: RemoteCapabilityState::NotProbed,
            sftp: RemoteCapabilityState::NotProbed,
            sftp_namespace: SftpNamespaceKind::Unknown,
            sftp_canonical_path: None,
            sftp_transport: None,
            diagnostic: RemoteCapabilityState::NotProbed,
            diagnostic_provider: None,
            diagnostic_message: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOperationClass {
    PassiveRead,
    ActiveProbe,
    StateChange,
    Unbounded,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshServiceName(String);

impl SshServiceName {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() || value.len() > 128 {
            return Err("服务名长度必须是 1 - 128 bytes".into());
        }
        let mut bytes = value.bytes();
        if !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !bytes.all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'@' | b'-')
            })
        {
            return Err(
                "服务名只能包含字母、数字、点、下划线、@ 和连字符，且必须以字母或数字开头".into(),
            );
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshLogSource {
    System,
    Application,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticTimeRange {
    minutes: u16,
}

impl DiagnosticTimeRange {
    pub fn last_minutes(minutes: u16) -> Result<Self, String> {
        if !(1..=1_440).contains(&minutes) {
            return Err("诊断时间范围必须是最近 1 - 1440 分钟".into());
        }
        Ok(Self { minutes })
    }

    pub fn minutes(self) -> u16 {
        self.minutes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshDiagnosticOperation {
    SystemOverview,
    ResourceSnapshot,
    ProcessList,
    NetworkSnapshot,
    DiskOverview,
    FileMetadata {
        path: RemotePath,
    },
    FileChunk {
        path: RemotePath,
        position: RemoteFileChunkPosition,
    },
    LogQuery {
        source: SshLogSource,
        service: Option<SshServiceName>,
        max_items: u16,
        since: Option<DiagnosticTimeRange>,
    },
    ServiceStatus {
        name: SshServiceName,
    },
}

impl SshDiagnosticOperation {
    pub fn class(&self) -> DiagnosticOperationClass {
        DiagnosticOperationClass::PassiveRead
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::SystemOverview => "system_overview",
            Self::ResourceSnapshot => "resource_snapshot",
            Self::ProcessList => "process_list",
            Self::NetworkSnapshot => "network_snapshot",
            Self::DiskOverview => "disk_overview",
            Self::FileMetadata { .. } => "file_metadata",
            Self::FileChunk { .. } => "file_chunk",
            Self::LogQuery { .. } => "log_query",
            Self::ServiceStatus { .. } => "service_status",
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.class() != DiagnosticOperationClass::PassiveRead {
            return Err("诊断策略只允许有界被动读取".into());
        }
        match self {
            Self::LogQuery {
                source,
                service,
                max_items,
                ..
            } => {
                if !(1..=MAX_DIAGNOSTIC_ITEMS as u16).contains(max_items) {
                    return Err(format!("日志条数必须是 1 - {MAX_DIAGNOSTIC_ITEMS}"));
                }
                if (*source == SshLogSource::Service) != service.is_some() {
                    return Err("服务日志必须且只能提供精确服务名".into());
                }
                Ok(())
            }
            Self::FileMetadata { path } | Self::FileChunk { path, .. } => {
                if path.namespace() == SftpNamespaceKind::Unknown {
                    return Err("文件诊断需要已识别的 SFTP 路径命名空间".into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticTermination {
    Completed,
    TimedOut,
    OutputLimitExceeded,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticErrorCode {
    ProductionRequired,
    RemotePlatformUnknown,
    RemotePlatformMismatch,
    SftpUnavailable,
    SftpNamespaceUnsupported,
    RemotePathInvalid,
    DiagnosticProviderUnavailable,
    DiagnosticOperationUnsupported,
    DiagnosticPolicyDenied,
    DiagnosticTimeout,
    DiagnosticOutputLimitExceeded,
    DiagnosticProtocolInvalid,
    GatewayVersionMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshDiagnosticResult {
    pub profile_id: SshProfileId,
    pub operation: String,
    pub operating_system: RemoteOperatingSystem,
    pub provider: SshDiagnosticProviderKind,
    /// UTF-8 文本或紧凑 JSON；基础设施层已移除终端控制字符并执行大小限制。
    pub output: String,
    pub exit_code: Option<i32>,
    pub termination: DiagnosticTermination,
    pub truncated: bool,
    pub elapsed_millis: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DiagnosticCancellation(Arc<AtomicBool>);

impl DiagnosticCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_name_rejects_shell_syntax_and_options() -> Result<(), String> {
        for value in ["-sshd", "sshd;reboot", "$(id)", "name value", "服务"] {
            assert!(SshServiceName::parse(value).is_err(), "{value}");
        }
        assert_eq!(
            SshServiceName::parse("ssh.service")?.as_str(),
            "ssh.service"
        );
        Ok(())
    }

    #[test]
    fn log_request_requires_bounded_items_and_matching_service() {
        let missing_service = SshDiagnosticOperation::LogQuery {
            source: SshLogSource::Service,
            service: None,
            max_items: 100,
            since: None,
        };
        assert!(missing_service.validate().is_err());

        let too_many = SshDiagnosticOperation::LogQuery {
            source: SshLogSource::System,
            service: None,
            max_items: 5_001,
            since: None,
        };
        assert!(too_many.validate().is_err());
    }
}
