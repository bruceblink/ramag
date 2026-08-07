use super::remote::{resolved_new_remote_path, resolved_remote_path};
use super::*;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, JumpServerAccount, JumpServerAsset, JumpServerAssetDetail,
    JumpServerCatalog, JumpServerConnection, JumpServerCredential, JumpServerOrganization,
    JumpServerRdpSession, JumpServerSession, QueryRecord, QueryRecordId, SshAuthMode,
    SshPathFavorites, SshProfileOrigin, SshProgressFn, SshWorkspaceState,
};
use ramag_domain::error::READ_ONLY_MESSAGE;
use ramag_domain::traits::JumpServerDriver;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct NoopStorage {
    preferences: Mutex<HashMap<String, String>>,
    ssh_profiles: Mutex<HashMap<SshProfileId, SshProfile>>,
}

#[async_trait::async_trait]
impl Storage for NoopStorage {
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

    async fn get_ssh_profile(&self, id: &SshProfileId) -> Result<Option<SshProfile>> {
        Ok(self.ssh_profiles.lock().get(id).cloned())
    }

    async fn save_ssh_profile(&self, profile: &SshProfile) -> Result<()> {
        self.ssh_profiles
            .lock()
            .insert(profile.id.clone(), profile.clone());
        Ok(())
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
        Ok(self.preferences.lock().get(key).cloned())
    }

    async fn set_preference(&self, key: &str, value: &str) -> Result<()> {
        self.preferences
            .lock()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn delete_preference(&self, key: &str) -> Result<()> {
        self.preferences.lock().remove(key);
        Ok(())
    }

    async fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(plain.to_vec())
    }

    async fn unseal(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(cipher.to_vec())
    }
}

struct TerminalDriver;

struct CountingJumpServerDriver {
    detail_calls: Arc<AtomicUsize>,
    web_session_calls: Arc<AtomicUsize>,
}

#[tokio::test]
async fn ssh_module_settings_are_persisted_and_restored() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let settings = SshModuleSettings {
        windows_sftp_compatibility: true,
    };

    service.save_module_settings(&settings).await.unwrap();

    let restored = SshService::new(Arc::new(TerminalDriver), storage);
    assert_eq!(restored.load_module_settings().await.unwrap(), settings);
}

#[tokio::test]
async fn windows_compatibility_setting_only_changes_windows_or_auto_profiles() {
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));
    service
        .save_module_settings(&SshModuleSettings {
            windows_sftp_compatibility: true,
        })
        .await
        .unwrap();
    let mut linux = SshProfile::new("linux", "linux.example");
    linux.remote_platform = RemotePlatformPreference::Linux;
    let windows = SshProfile {
        remote_platform: RemotePlatformPreference::Windows,
        ..SshProfile::new("windows", "windows.example")
    };
    let auto = SshProfile::new("auto", "auto.example");

    assert!(
        !service
            .apply_module_settings(&linux)
            .windows_sftp_compatibility
    );
    assert!(
        service
            .apply_module_settings(&windows)
            .windows_sftp_compatibility
    );
    assert!(
        service
            .apply_module_settings(&auto)
            .windows_sftp_compatibility
    );
}

#[async_trait::async_trait]
impl JumpServerDriver for CountingJumpServerDriver {
    async fn authenticate(&self, credential: &JumpServerCredential) -> Result<JumpServerSession> {
        Ok(JumpServerSession {
            base_url: credential.base_url.clone(),
            ssh_host: "jump.example.com".into(),
            ssh_port: credential.ssh_port,
            username: credential.username.clone(),
            password: credential.password.clone(),
            token_keyword: "Bearer".into(),
            token: "token".into(),
            organizations: vec![JumpServerOrganization {
                id: "org-1".into(),
                name: "DEFAULT".into(),
            }],
        })
    }

    async fn load_catalog(&self, _session: &JumpServerSession) -> Result<JumpServerCatalog> {
        Ok(JumpServerCatalog {
            assets: vec![jumpserver_asset()],
            nodes: Vec::new(),
        })
    }

    async fn asset_detail(
        &self,
        _session: &JumpServerSession,
        asset: &JumpServerAsset,
    ) -> Result<JumpServerAssetDetail> {
        self.detail_calls.fetch_add(1, Ordering::SeqCst);
        Ok(JumpServerAssetDetail {
            asset: asset.clone(),
            accounts: vec![JumpServerAccount {
                id: "account-1".into(),
                alias: "account-1".into(),
                name: "root".into(),
                username: "root".into(),
                has_secret: true,
                can_connect: true,
            }],
            ssh_enabled: true,
            rdp_web_enabled: true,
        })
    }

    async fn create_rdp_web_session(
        &self,
        _session: &JumpServerSession,
        _asset: &JumpServerAsset,
        _account: &JumpServerAccount,
    ) -> Result<String> {
        self.web_session_calls.fetch_add(1, Ordering::SeqCst);
        Ok("https://jump.example.com/lion/connect?token=session-token".into())
    }
}

fn jumpserver_asset() -> JumpServerAsset {
    JumpServerAsset {
        id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        name: "taiyuan-login".into(),
        address: "tycs.example.com".into(),
        platform: "Linux".into(),
        labels: Vec::new(),
        node_ids: Vec::new(),
        favorite: false,
        ungrouped: false,
        active: true,
    }
}

fn jumpserver_session() -> JumpServerSession {
    JumpServerSession {
        base_url: "https://jump.example.com/".into(),
        ssh_host: "jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "login-password".into(),
        token_keyword: "Bearer".into(),
        token: "token".into(),
        organizations: Vec::new(),
    }
}

#[async_trait::async_trait]
impl SshDriver for TerminalDriver {
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
        Ok(SshLaunchCommand {
            profile_id: profile.id.clone(),
            authorization_generation: 0,
            program: "/mock/ssh".into(),
            args: vec!["--".into(), profile.host.clone()],
            env: HashMap::new(),
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
        Ok(SshRemoteCapabilities {
            openssh_client: RemoteCapabilityState::Available,
            ssh_authentication: RemoteCapabilityState::Available,
            operating_system: match profile.remote_platform {
                RemotePlatformPreference::Windows => RemoteOperatingSystem::Windows,
                RemotePlatformPreference::Auto | RemotePlatformPreference::Linux => {
                    RemoteOperatingSystem::Linux
                }
            },
            ssh_execution: RemoteCapabilityState::Available,
            terminal: RemoteCapabilityState::Available,
            sftp: RemoteCapabilityState::Available,
            sftp_namespace: ramag_domain::entities::SftpNamespaceKind::Posix,
            sftp_canonical_path: Some(
                ramag_domain::entities::RemotePath::parse_server_canonical("/").unwrap(),
            ),
            diagnostic: RemoteCapabilityState::Available,
            diagnostic_provider: Some(
                ramag_domain::entities::SshDiagnosticProviderKind::LinuxBuiltinV1,
            ),
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
                .ok_or_else(|| DomainError::Other("missing provider".into()))?,
            output: "ok".into(),
            exit_code: Some(0),
            termination: ramag_domain::entities::DiagnosticTermination::Completed,
            truncated: false,
            elapsed_millis: 1,
        })
    }

    async fn list_directory(&self, _profile: &SshProfile, path: &str) -> Result<RemoteDirectory> {
        Ok(RemoteDirectory {
            path: path.into(),
            entries: Vec::new(),
        })
    }

    async fn read_file_preview(
        &self,
        _profile: &SshProfile,
        _path: &str,
    ) -> Result<RemoteFilePreview> {
        Ok(RemoteFilePreview {
            bytes: b"preview".to_vec(),
            total_bytes: 7,
            truncated: false,
        })
    }

    async fn read_file_chunk(
        &self,
        _profile: &SshProfile,
        _path: &str,
        _position: RemoteFileChunkPosition,
    ) -> Result<RemoteFileChunk> {
        Ok(RemoteFileChunk {
            bytes: b"preview".to_vec(),
            offset: 0,
            total_bytes: 7,
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
        _overwrite: OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Ok(())
    }

    async fn download(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Ok(())
    }

    async fn download_directory(
        &self,
        _profile: &SshProfile,
        _remote_path: &str,
        _local_path: &Path,
        _overwrite: OverwritePolicy,
        _cancellation: TransferCancellation,
        _progress: SshProgressFn,
    ) -> Result<()> {
        Ok(())
    }

    async fn disconnect(&self, _profile_id: &SshProfileId) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn remote_paths_follow_probed_namespace_and_windows_name_rules() {
    let windows_virtual = SshRemoteCapabilities {
        operating_system: RemoteOperatingSystem::Windows,
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: ramag_domain::entities::SftpNamespaceKind::Virtual,
        sftp_canonical_path: Some(
            ramag_domain::entities::RemotePath::parse_with_namespace(
                "/Users/Admin",
                ramag_domain::entities::SftpNamespaceKind::Virtual,
            )
            .unwrap(),
        ),
        ..SshRemoteCapabilities::default()
    };
    assert_eq!(
        resolved_remote_path(&windows_virtual, ".").unwrap(),
        "/Users/Admin"
    );
    assert!(resolved_new_remote_path(&windows_virtual, "/Users/Admin/CON.txt").is_err());
    assert!(resolved_remote_path(&windows_virtual, "C:/Users/Admin").is_err());

    let windows_drive = SshRemoteCapabilities {
        operating_system: RemoteOperatingSystem::Windows,
        sftp: RemoteCapabilityState::Available,
        sftp_namespace: ramag_domain::entities::SftpNamespaceKind::WindowsDrive,
        ..SshRemoteCapabilities::default()
    };
    assert!(resolved_remote_path(&windows_drive, "D:/Data/中文.txt").is_ok());
    assert!(resolved_new_remote_path(&windows_drive, "D:/Data/file.txt:stream").is_err());
}

#[test]
fn workspace_preference_deduplicates_and_repairs_active_profile() {
    let first = SshProfileId::new();
    let missing = SshProfileId::new();
    let json = serde_json::to_string(&SshWorkspacePreference {
        workspaces: vec![
            SshWorkspaceState {
                profile_id: first.clone(),
                last_remote_path: "/home".into(),
            },
            SshWorkspaceState {
                profile_id: first.clone(),
                last_remote_path: "/duplicate".into(),
            },
        ],
        active_profile_id: Some(missing),
        path_favorites: vec![SshPathFavorites {
            profile_id: first.clone(),
            paths: vec!["/home".into(), "/home".into(), "/var/log".into()],
        }],
    })
    .unwrap();

    let parsed = parse_workspace_preference(&json).unwrap();
    assert_eq!(parsed.workspaces.len(), 1);
    assert!(parsed.active_profile_id.is_none());
    assert_eq!(parsed.path_favorites[0].paths, ["/home", "/var/log"]);
}

#[test]
fn workspace_preference_accepts_legacy_data_and_rejects_relative_favorites() {
    let legacy = r#"{"workspaces":[],"active_profile_id":null}"#;
    let parsed = parse_workspace_preference(legacy).unwrap();
    assert!(parsed.path_favorites.is_empty());

    let invalid = serde_json::to_string(&SshWorkspacePreference {
        path_favorites: vec![SshPathFavorites {
            profile_id: SshProfileId::new(),
            paths: vec!["var/log".into()],
        }],
        ..SshWorkspacePreference::default()
    })
    .unwrap();
    assert!(parse_workspace_preference(&invalid).is_err());
}

#[test]
fn workspace_preference_accepts_windows_drive_favorites() {
    let preference = SshWorkspacePreference {
        path_favorites: vec![SshPathFavorites {
            profile_id: SshProfileId::new(),
            paths: vec!["C:/Users/Administrator".into(), "D:/Data".into()],
        }],
        ..SshWorkspacePreference::default()
    };

    let normalized = normalized_workspace_preference(preference).unwrap();
    assert_eq!(
        normalized.path_favorites[0].paths,
        ["C:/Users/Administrator", "D:/Data"]
    );
}

mod jumpserver_tests;
mod transfer_tests;
