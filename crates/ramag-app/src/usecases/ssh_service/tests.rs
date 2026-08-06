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
            terminal: if profile.production {
                RemoteCapabilityState::BlockedByPolicy
            } else {
                RemoteCapabilityState::Available
            },
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

#[test]
fn transfer_store_enforces_queue_and_terminal_history_bounds() {
    let store = TransferStore::new();
    let profile_id = SshProfileId::new();
    for index in 0..MAX_TRANSFER_HISTORY + 10 {
        let id = store
            .enqueue(TransferTask::new(
                profile_id.clone(),
                TransferDirection::Upload,
                format!("/tmp/{index}"),
                format!("/remote/{index}"),
            ))
            .unwrap();
        let _ = store.begin(&id).unwrap();
        store.finish(&id, &Ok(()), false);
    }
    let state = store.state.lock();
    assert_eq!(
        state
            .tasks
            .iter()
            .filter(|task| task.status.is_terminal())
            .count(),
        MAX_TRANSFER_HISTORY
    );
}

#[test]
fn transfer_store_rejects_more_than_bounded_active_tasks() {
    let store = TransferStore::new();
    let profile_id = SshProfileId::new();
    for index in 0..MAX_QUEUED_TRANSFERS {
        store
            .enqueue(TransferTask::new(
                profile_id.clone(),
                TransferDirection::Download,
                format!("/tmp/{index}"),
                format!("/remote/{index}"),
            ))
            .unwrap();
    }
    let error = store
        .enqueue(TransferTask::new(
            profile_id,
            TransferDirection::Download,
            "/tmp/overflow",
            "/remote/overflow",
        ))
        .unwrap_err();
    assert!(error.message().contains("上限"));
}

#[test]
fn cancelled_running_transfer_finishes_as_cancelled() {
    let store = TransferStore::new();
    let id = store
        .enqueue(TransferTask::new(
            SshProfileId::new(),
            TransferDirection::Upload,
            "/tmp/source",
            "/remote/target",
        ))
        .unwrap();
    let (_, cancellation) = store.begin(&id).unwrap();
    cancellation.cancel();
    store.finish(
        &id,
        &Err(DomainError::Other("传输已取消".into())),
        cancellation.is_cancelled(),
    );

    let state = store.state.lock();
    let task = state.tasks.iter().find(|task| task.id == id).unwrap();
    assert_eq!(task.status, TransferStatus::Cancelled);
}

#[test]
fn queued_transfer_is_cancelled_without_waiting_for_executor() {
    let store = TransferStore::new();
    let id = store
        .enqueue(TransferTask::new(
            SshProfileId::new(),
            TransferDirection::Download,
            "/tmp/target",
            "/remote/source",
        ))
        .unwrap();
    let mut state = store.state.lock();
    cancel_tasks(&mut state, std::slice::from_ref(&id));

    let task = state.tasks.iter().find(|task| task.id == id).unwrap();
    assert_eq!(task.status, TransferStatus::Cancelled);
    assert!(!state.cancellations.contains_key(&id));
}

#[test]
fn production_profile_blocks_terminal_and_sftp_writes() {
    let mut profile = SshProfile::new("production", "server.example");
    profile.production = true;
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));

    futures::executor::block_on(service.save_profile(&profile)).unwrap();
    assert!(matches!(
        futures::executor::block_on(service.terminal_command(&profile.id, None)),
        Err(DomainError::Forbidden(message)) if message.contains("生产模式")
    ));

    let preview =
        futures::executor::block_on(service.read_file_preview(&profile, "/readme.txt")).unwrap();
    assert_eq!(preview.bytes, b"preview");
    let chunk = futures::executor::block_on(service.read_file_chunk(
        &profile,
        "/readme.txt",
        RemoteFileChunkPosition::Tail,
    ))
    .unwrap();
    assert_eq!(chunk.bytes, b"preview");
    futures::executor::block_on(service.list_directory(&profile, "/")).unwrap();
    service
        .enqueue_download(&profile, "/readme.txt", Path::new("/tmp/readme.txt"))
        .unwrap();

    assert!(matches!(
        futures::executor::block_on(service.create_directory(&profile, "/new-directory")),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
    assert!(matches!(
        futures::executor::block_on(service.save_file(
            &profile,
            "/readme.txt",
            b"preview",
            b"changed"
        )),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
    assert!(matches!(
        futures::executor::block_on(service.rename(
            &profile,
            "/readme.txt",
            "/renamed.txt"
        )),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
    assert!(matches!(
        futures::executor::block_on(service.remove(
            &profile,
            "/readme.txt",
            RemoteEntryKind::File
        )),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
    assert!(matches!(
        service.enqueue_upload(
            &profile,
            Path::new("/tmp/readme.txt"),
            "/readme.txt"
        ),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));

    let mut writable_profile = profile.clone();
    writable_profile.production = false;
    let queued = service
        .enqueue_upload(
            &writable_profile,
            Path::new("/tmp/readme.txt"),
            "/readme.txt",
        )
        .unwrap();
    assert!(matches!(
        futures::executor::block_on(service.execute_transfer(&queued, OverwritePolicy::Refuse)),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
}

#[test]
fn terminal_generation_invalidates_command_before_pty_start() {
    let profile = SshProfile::new("server", "server.example");
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));
    futures::executor::block_on(service.save_profile(&profile)).unwrap();

    let command = futures::executor::block_on(service.terminal_command(&profile.id, None)).unwrap();
    assert!(service.terminal_launch_is_current(&command));
    service.block_terminal_launches(&profile.id);
    assert!(!service.terminal_launch_is_current(&command));
    assert!(matches!(
        futures::executor::block_on(service.terminal_command(&profile.id, None)),
        Err(DomainError::Forbidden(_))
    ));
}

#[test]
fn interactive_windows_terminal_evidence_promotes_auto_sftp_to_virtual_root() {
    let mut profile = SshProfile::new("windows", "jump.example.com");
    profile.origin = SshProfileOrigin::JumpServer;
    profile.username = "gateway#Administrator#asset-1".into();
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));

    let capabilities = service
        .remember_terminal_windows(&profile, RemoteShellKind::Cmd)
        .unwrap();

    assert_eq!(
        capabilities.operating_system,
        RemoteOperatingSystem::Windows
    );
    assert_eq!(capabilities.shell, RemoteShellKind::Cmd);
    assert_eq!(capabilities.sftp_namespace, SftpNamespaceKind::Virtual);
    assert_eq!(
        capabilities
            .sftp_canonical_path
            .as_ref()
            .unwrap()
            .canonical(),
        "/"
    );
    assert_eq!(
        super::remote::profile_for_capabilities(&profile, &capabilities).remote_platform,
        RemotePlatformPreference::Windows
    );
}

#[test]
fn production_diagnostic_uses_current_profile_and_fixed_operation() {
    let mut profile = SshProfile::new("production", "server.example");
    profile.production = true;
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));
    futures::executor::block_on(service.save_profile(&profile)).unwrap();

    let result = futures::executor::block_on(service.execute_diagnostic(
        &profile.id,
        &SshDiagnosticOperation::SystemOverview,
        DiagnosticCancellation::default(),
    ))
    .unwrap();
    assert_eq!(result.operation, "system_overview");
    assert_eq!(result.output, "ok");
}

#[test]
fn remote_editor_rejects_content_above_preview_bound() {
    let profile = SshProfile::new("server", "server.example");
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));
    futures::executor::block_on(service.save_profile(&profile)).unwrap();
    let oversized = vec![b'a'; MAX_REMOTE_FILE_PREVIEW_BYTES + 1];

    assert!(matches!(
        futures::executor::block_on(service.save_file(
            &profile,
            "/readme.txt",
            b"preview",
            &oversized
        )),
        Err(DomainError::InvalidConfig(message)) if message.contains("2 MiB")
    ));
}

#[test]
fn directory_download_is_queued_as_archive() {
    let profile = SshProfile::new("server", "server.example");
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));
    let local = std::env::temp_dir().join("ramag-directory-download-test.tar.gz");

    let id = service
        .enqueue_directory_download(&profile, "/srv/logs", &local)
        .unwrap();
    let task = service
        .transfer_tasks()
        .into_iter()
        .find(|task| task.id == id)
        .unwrap();
    assert_eq!(task.direction, TransferDirection::DownloadArchive);
}

#[test]
fn jumpserver_test_and_save_refresh_asset_detail() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: Arc::new(AtomicUsize::new(0)),
    });
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()))
        .with_jumpserver_driver(jumpserver);
    let session = jumpserver_session();
    let asset = jumpserver_asset();

    let tested =
        futures::executor::block_on(service.test_jumpserver_asset(&session, &asset, "account-1"))
            .unwrap();
    let saved = futures::executor::block_on(service.save_jumpserver_asset_for_connection(
        "00000000-0000-0000-0000-000000000002",
        &session,
        &asset,
        "account-1",
    ))
    .unwrap();

    assert_eq!(detail_calls.load(Ordering::SeqCst), 2);
    for profile in [&tested, &saved] {
        assert_eq!(profile.host, "jump.example.com");
        assert_eq!(profile.port, Some(2222));
        assert_eq!(
            profile.username,
            "alice#root#00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(profile.auth_mode, SshAuthMode::Password);
        assert_eq!(profile.origin, SshProfileOrigin::JumpServer);
        assert_eq!(profile.password, "login-password");
    }
    assert!(tested.jumpserver_rdp_session.is_none());
    let rdp_session = saved
        .jumpserver_rdp_session
        .as_ref()
        .expect("导入时应记录可复用的远程桌面目标");
    assert_eq!(
        rdp_session.connection_id,
        "00000000-0000-0000-0000-000000000002"
    );
    assert_eq!(rdp_session.account_id, "account-1");
}

#[test]
fn jumpserver_rdp_web_session_refreshes_detail_and_uses_selected_account() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let web_session_calls = Arc::new(AtomicUsize::new(0));
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: web_session_calls.clone(),
    });
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()))
        .with_jumpserver_driver(jumpserver);

    let url = futures::executor::block_on(service.create_jumpserver_rdp_web_session(
        &jumpserver_session(),
        &jumpserver_asset(),
        "account-1",
    ))
    .unwrap();

    assert_eq!(
        url,
        "https://jump.example.com/lion/connect?token=session-token"
    );
    assert_eq!(detail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(web_session_calls.load(Ordering::SeqCst), 1);
}

fn jumpserver_rdp_record(connection_id: String) -> JumpServerRdpSession {
    JumpServerRdpSession {
        connection_id,
        jumpserver_url: "https://jump.example.com".into(),
        asset_id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        asset_name: "windows-prod".into(),
        asset_address: "10.0.0.2".into(),
        asset_platform: "Windows".into(),
        account_id: "account-1".into(),
        account_name: "admin".into(),
        account_username: "Administrator".into(),
    }
}

#[test]
fn jumpserver_rdp_history_is_encrypted_and_supports_favorites() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let record = jumpserver_rdp_record("00000000-0000-0000-0000-000000000010".into());

    let recent =
        futures::executor::block_on(service.record_jumpserver_rdp_session(record.clone())).unwrap();
    assert_eq!(recent.recent, vec![record.clone()]);
    let stored = storage
        .preferences
        .lock()
        .get("ssh_jumpserver_rdp_sessions_v1")
        .cloned()
        .unwrap();
    assert!(stored.starts_with("enc-v1:"));
    assert!(!stored.contains("windows-prod"));

    let favorite =
        futures::executor::block_on(service.set_jumpserver_rdp_session_favorite(&record, true))
            .unwrap();
    assert_eq!(favorite.favorites, vec![record.clone()]);
    assert!(favorite.recent.is_empty());

    let recent_again =
        futures::executor::block_on(service.set_jumpserver_rdp_session_favorite(&record, false))
            .unwrap();
    assert!(recent_again.favorites.is_empty());
    assert_eq!(recent_again.recent, vec![record]);
}

#[test]
fn saved_jumpserver_rdp_session_reauthenticates_and_revalidates_target() {
    let detail_calls = Arc::new(AtomicUsize::new(0));
    let web_session_calls = Arc::new(AtomicUsize::new(0));
    let storage = Arc::new(NoopStorage::default());
    let jumpserver: Arc<dyn JumpServerDriver> = Arc::new(CountingJumpServerDriver {
        detail_calls: detail_calls.clone(),
        web_session_calls: web_session_calls.clone(),
    });
    let service =
        SshService::new(Arc::new(TerminalDriver), storage).with_jumpserver_driver(jumpserver);
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "secret-password".into(),
    };
    let connection =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    let record = jumpserver_rdp_record(connection.id);

    let url = futures::executor::block_on(service.create_saved_jumpserver_rdp_web_session(&record))
        .unwrap();

    assert_eq!(
        url,
        "https://jump.example.com/lion/connect?token=session-token"
    );
    assert_eq!(detail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(web_session_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn jumpserver_profile_uses_connect_permission_not_managed_secret_flag() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: false,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: false,
    };

    assert!(
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .is_ok()
    );
    detail.accounts[0].can_connect = false;
    assert!(
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .is_err()
    );
}

#[test]
fn jumpserver_windows_asset_preserves_remote_platform_preference() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "administrator".into(),
            username: "Administrator".into(),
            has_secret: true,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: true,
    };
    detail.asset.platform = "Windows".into();

    let profile =
        super::jumpserver::build_jumpserver_profile(&jumpserver_session(), &detail, "account-1")
            .unwrap();

    assert_eq!(profile.remote_platform, RemotePlatformPreference::Windows);
    assert_eq!(profile.rdp_web_enabled, Some(true));
}

#[test]
fn jumpserver_profile_omits_unavailable_remote_desktop_target() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            alias: "account-1".into(),
            name: "administrator".into(),
            username: "Administrator".into(),
            has_secret: true,
            can_connect: true,
        }],
        ssh_enabled: true,
        rdp_web_enabled: false,
    };
    let session = jumpserver_session();
    let mut profile =
        super::jumpserver::build_jumpserver_profile(&session, &detail, "account-1").unwrap();

    super::jumpserver::attach_jumpserver_rdp_session(
        &mut profile,
        "00000000-0000-0000-0000-000000000002",
        &session,
        &detail,
        "account-1",
    )
    .unwrap();
    assert!(profile.jumpserver_rdp_session.is_none());

    detail.rdp_web_enabled = true;
    detail.accounts[0].has_secret = false;
    super::jumpserver::attach_jumpserver_rdp_session(
        &mut profile,
        "00000000-0000-0000-0000-000000000002",
        &session,
        &detail,
        "account-1",
    )
    .unwrap();
    assert!(profile.jumpserver_rdp_session.is_none());
}

#[test]
fn jumpserver_connections_are_encrypted_updated_and_deleted() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "secret-password".into(),
    };

    let first =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    let stored = storage
        .preferences
        .lock()
        .get("ssh_jumpserver_connections_v2")
        .cloned()
        .unwrap();
    assert!(stored.starts_with("enc-v1:"));
    assert!(!stored.contains("secret-password"));
    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded, vec![first.clone()]);

    let mut updated_credential = credential;
    updated_credential.ssh_port = 2200;
    let updated = futures::executor::block_on(
        service.save_jumpserver_connection(Some(&first.id), &updated_credential),
    )
    .unwrap();
    assert_eq!(updated.id, first.id);
    assert_eq!(updated.credential.ssh_port, 2200);

    futures::executor::block_on(service.delete_jumpserver_connection(&first.id)).unwrap();
    assert!(
        futures::executor::block_on(service.load_jumpserver_connections())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn jumpserver_connections_deduplicate_same_login_and_keep_latest_password() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let mut credential = JumpServerCredential {
        base_url: "https://jump.example.com/".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "old-password".into(),
    };

    let first =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();
    credential.base_url = "HTTPS://JUMP.EXAMPLE.COM".into();
    credential.password = "new-password".into();
    let updated =
        futures::executor::block_on(service.save_jumpserver_connection(None, &credential)).unwrap();

    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(updated.id, first.id);
    assert_eq!(loaded[0].credential.password, "new-password");
}

#[test]
fn jumpserver_connections_remove_existing_duplicate_records_when_loading() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://jump.example.com".into(),
        ssh_port: 2222,
        username: "alice".into(),
        password: "new-password".into(),
    };
    let newest = JumpServerConnection::new(credential.clone());
    let mut older_credential = credential;
    older_credential.password = "old-password".into();
    let older = JumpServerConnection::new(older_credential);
    let encoded = format!(
        "enc-v1:{}",
        hex::encode(serde_json::to_vec(&vec![newest.clone(), older]).unwrap())
    );
    storage
        .preferences
        .lock()
        .insert("ssh_jumpserver_connections_v2".into(), encoded);

    let loaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(loaded, vec![newest]);
    let reloaded = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(reloaded.len(), 1);
}

#[test]
fn jumpserver_legacy_credential_is_migrated_to_connection_list() {
    let storage = Arc::new(NoopStorage::default());
    let service = SshService::new(Arc::new(TerminalDriver), storage.clone());
    let credential = JumpServerCredential {
        base_url: "https://legacy.example.com".into(),
        ssh_port: 2222,
        username: "legacy".into(),
        password: "secret-password".into(),
    };
    let encoded = format!(
        "enc-v1:{}",
        hex::encode(serde_json::to_vec(&credential).unwrap())
    );
    storage
        .preferences
        .lock()
        .insert("ssh_jumpserver_credential_v1".into(), encoded);

    let migrated = futures::executor::block_on(service.load_jumpserver_connections()).unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].credential, credential);
    assert!(JumpServerConnection::validate(&migrated[0]).is_ok());
    let preferences = storage.preferences.lock();
    assert!(!preferences.contains_key("ssh_jumpserver_credential_v1"));
    assert!(preferences.contains_key("ssh_jumpserver_connections_v2"));
}
