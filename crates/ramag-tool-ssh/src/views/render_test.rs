#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::Duration;

use async_trait::async_trait;
#[cfg(unix)]
use gpui::Focusable as _;
use gpui::{
    AppContext as _, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
    size,
};
use ramag_app::SshService;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DiagnosticCancellation, DiagnosticTermination,
    JumpServerAccount, JumpServerAsset, JumpServerAssetDetail, JumpServerCatalog,
    JumpServerConnection, JumpServerCredential, JumpServerRdpSession, JumpServerRdpSessionHistory,
    JumpServerSession, QueryRecord, QueryRecordId, RemoteCapabilityState, RemoteDirectory,
    RemoteEntry, RemoteEntryKind, RemoteOperatingSystem, RemotePlatformPreference, RemoteShellKind,
    SftpNamespaceKind, SshAuthMode, SshCapability, SshDiagnosticOperation,
    SshDiagnosticProviderKind, SshDiagnosticResult, SshLaunchCommand, SshPathFavorites, SshProfile,
    SshProfileId, SshProfileOrigin, SshProgressFn, SshRemoteCapabilities, SshWorkspacePreference,
    SshWorkspaceState, TransferCancellation,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{JumpServerDriver, SshDriver, Storage};
#[cfg(unix)]
use ramag_terminal::{TerminalCommand, TerminalCore, TerminalView};

use super::SshView;
use super::jumpserver_dialog::JumpServerPanel;
#[cfg(unix)]
use super::model::TerminalTab;
use super::model::{Notice, ViewMode};
use super::profile_dialog::SshProfileFormPanel;
use super::remote_session_dialog::RemoteSessionPanel;

struct MockStorage {
    profiles: Vec<SshProfile>,
    workspace_preference: Option<String>,
    preferences: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl Storage for MockStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }

    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }

    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }

    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn list_ssh_profiles(&self) -> Result<Vec<SshProfile>> {
        Ok(self.profiles.clone())
    }

    async fn get_ssh_profile(&self, id: &SshProfileId) -> Result<Option<SshProfile>> {
        Ok(self
            .profiles
            .iter()
            .find(|profile| &profile.id == id)
            .cloned())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, key: &str) -> Result<Option<String>> {
        if key == "ssh_workspaces_v1" {
            return Ok(self.workspace_preference.clone());
        }
        Ok(self
            .preferences
            .lock()
            .expect("mock preferences should not be poisoned")
            .get(key)
            .cloned())
    }

    async fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.preferences
            .lock()
            .expect("mock preferences should not be poisoned")
            .insert(key.into(), value.into());
        Ok(())
    }

    async fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(plain.to_vec())
    }

    async fn unseal(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(cipher.to_vec())
    }
}

#[derive(Default)]
struct MockSshDriver {
    working_terminal: bool,
}

struct MockJumpServerDriver;

#[async_trait]
impl JumpServerDriver for MockJumpServerDriver {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession> {
        Ok(JumpServerSession {
            base_url: credential.base_url.clone(),
            ssh_host: "jump.example.com".into(),
            ssh_port: credential.ssh_port,
            username: credential.username.clone(),
            password: credential.password.clone(),
            token_keyword: "Bearer".into(),
            token: "token".into(),
            organizations: Vec::new(),
        })
    }

    async fn load_catalog(&self, _session: &JumpServerSession) -> Result<JumpServerCatalog> {
        Err(DomainError::NotImplemented("mock catalog".into()))
    }

    async fn asset_detail(
        &self,
        _session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        Ok(JumpServerAssetDetail {
            asset: asset.clone(),
            accounts: vec![JumpServerAccount {
                id: "account-1".into(),
                alias: "account-1".into(),
                name: "admin".into(),
                username: "Administrator".into(),
                has_secret: true,
                can_connect: true,
            }],
            ssh_enabled: false,
            rdp_web_enabled: true,
        })
    }

    async fn create_rdp_web_session(
        &self,
        _session: &JumpServerSession,
        _asset: &JumpServerAsset,
        _account: &JumpServerAccount,
    ) -> Result<String> {
        Ok(
            "https://jump.example.com/lion/connect?token=00000000-0000-0000-0000-000000000002"
                .into(),
        )
    }
}

#[async_trait]
impl SshDriver for MockSshDriver {
    async fn probe(&self, _custom_path: Option<&str>) -> Result<SshCapability> {
        Ok(SshCapability {
            executable: "/mock/ssh".into(),
            version: "OpenSSH_mock".into(),
        })
    }

    async fn terminal_command(
        &self,
        profile: &SshProfile,
        _initial_directory: Option<&str>,
    ) -> Result<SshLaunchCommand> {
        let (program, args) = if self.working_terminal {
            ("/bin/sh".into(), vec!["-c".into(), "exit 0".into()])
        } else {
            ("/mock/ssh".into(), vec!["--".into(), profile.host.clone()])
        };
        Ok(SshLaunchCommand {
            profile_id: profile.id.clone(),
            authorization_generation: 0,
            program,
            args,
            env: Default::default(),
        })
    }

    async fn report_terminal_launch_failure(&self, _executable: &str) {}

    async fn test_connection(&self, _profile: &SshProfile) -> Result<()> {
        Ok(())
    }

    async fn probe_remote_capabilities(
        &self,
        profile: &SshProfile,
    ) -> Result<SshRemoteCapabilities> {
        let windows = profile.remote_platform == RemotePlatformPreference::Windows;
        let operating_system = if windows {
            RemoteOperatingSystem::Windows
        } else {
            RemoteOperatingSystem::Linux
        };
        let namespace = if windows {
            SftpNamespaceKind::WindowsDrive
        } else {
            SftpNamespaceKind::Posix
        };
        let canonical_path = if windows {
            ramag_domain::entities::RemotePath::parse_server_canonical("C:/Users/Administrator")
                .unwrap()
        } else {
            ramag_domain::entities::RemotePath::parse_server_canonical("/").unwrap()
        };
        Ok(SshRemoteCapabilities {
            openssh_client: RemoteCapabilityState::Available,
            ssh_authentication: RemoteCapabilityState::Available,
            operating_system,
            shell: if windows {
                RemoteShellKind::Cmd
            } else {
                RemoteShellKind::Posix
            },
            ssh_execution: RemoteCapabilityState::Available,
            terminal: RemoteCapabilityState::Available,
            sftp: RemoteCapabilityState::Available,
            sftp_namespace: namespace,
            sftp_canonical_path: Some(canonical_path),
            diagnostic: RemoteCapabilityState::Available,
            diagnostic_provider: Some(if windows {
                SshDiagnosticProviderKind::WindowsPowerShellV1
            } else {
                SshDiagnosticProviderKind::LinuxBuiltinV1
            }),
            ..SshRemoteCapabilities::default()
        })
    }

    async fn execute_diagnostic(
        &self,
        profile: &SshProfile,
        capabilities: &SshRemoteCapabilities,
        operation: &SshDiagnosticOperation,
        _cancellation: DiagnosticCancellation,
    ) -> Result<SshDiagnosticResult> {
        Ok(SshDiagnosticResult {
            profile_id: profile.id.clone(),
            operation: operation.kind().into(),
            operating_system: capabilities.operating_system,
            provider: capabilities
                .diagnostic_provider
                .unwrap_or(SshDiagnosticProviderKind::LinuxBuiltinV1),
            output: "ok".into(),
            exit_code: Some(0),
            termination: DiagnosticTermination::Completed,
            truncated: false,
            elapsed_millis: 1,
        })
    }

    async fn list_directory(&self, profile: &SshProfile, path: &str) -> Result<RemoteDirectory> {
        if profile.remote_platform == RemotePlatformPreference::Windows && matches!(path, "." | "/")
        {
            return Ok(RemoteDirectory {
                path: "/".into(),
                entries: ["C", "D"]
                    .into_iter()
                    .map(|drive| RemoteEntry {
                        name: format!("{drive}:"),
                        path: format!("/{drive}:/"),
                        kind: RemoteEntryKind::Directory,
                        size: 0,
                        permissions: None,
                        modified_at: None,
                    })
                    .collect(),
            });
        }
        let path = if path == "." { "/" } else { path };
        Ok(RemoteDirectory {
            path: path.into(),
            entries: Vec::new(),
        })
    }

    async fn read_file_preview(
        &self,
        _profile: &SshProfile,
        _path: &str,
    ) -> Result<ramag_domain::entities::RemoteFilePreview> {
        Ok(ramag_domain::entities::RemoteFilePreview {
            bytes: b"preview".to_vec(),
            total_bytes: 7,
            truncated: false,
        })
    }

    async fn read_file_chunk(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _position: ramag_domain::entities::RemoteFileChunkPosition,
    ) -> Result<ramag_domain::entities::RemoteFileChunk> {
        Ok(ramag_domain::entities::RemoteFileChunk {
            bytes: b"readme".to_vec(),
            offset: 0,
            total_bytes: 6,
        })
    }

    async fn save_file(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _expected: &[u8],
        _contents: &[u8],
    ) -> Result<()> {
        Ok(())
    }

    async fn create_directory(&self, _profile: &SshProfile, _path: &str) -> Result<()> {
        Ok(())
    }

    async fn rename(&self, _profile: &SshProfile, _old_path: &str, _new_path: &str) -> Result<()> {
        Ok(())
    }

    async fn remove(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _kind: RemoteEntryKind,
    ) -> Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        _profile: &SshProfile,
        _local_path: &Path,
        _remote_path: &str,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Err(DomainError::NotImplemented("mock upload".into()))
    }

    async fn download(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Err(DomainError::NotImplemented("mock download".into()))
    }

    async fn disconnect(&self, _profile_id: &SshProfileId) -> Result<()> {
        Ok(())
    }

    async fn download_directory(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: ramag_domain::entities::OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

mod jumpserver_tests;
mod lifecycle_tests;
mod support;
mod workspace_tests;

use support::*;
