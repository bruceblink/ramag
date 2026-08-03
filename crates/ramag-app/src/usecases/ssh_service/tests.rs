use super::*;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, JumpServerAccount, JumpServerAsset, JumpServerAssetDetail,
    JumpServerCredential, JumpServerOrganization, JumpServerSession, QueryRecord, QueryRecordId,
    SshAuthMode, SshPathFavorites, SshProgressFn, SshWorkspaceState,
};
use ramag_domain::error::READ_ONLY_MESSAGE;
use ramag_domain::traits::JumpServerDriver;
use std::sync::atomic::{AtomicUsize, Ordering};

struct NoopStorage;

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

    async fn save_ssh_profile(&self, _profile: &SshProfile) -> Result<()> {
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

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

struct TerminalDriver;

struct CountingJumpServerDriver {
    detail_calls: Arc<AtomicUsize>,
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

    async fn list_assets(&self, _session: &JumpServerSession) -> Result<Vec<JumpServerAsset>> {
        Ok(vec![jumpserver_asset()])
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
                name: "root".into(),
                username: "root".into(),
                has_secret: true,
                can_connect: true,
            }],
            ssh_enabled: true,
        })
    }
}

fn jumpserver_asset() -> JumpServerAsset {
    JumpServerAsset {
        id: "00000000-0000-0000-0000-000000000001".into(),
        org_id: "org-1".into(),
        name: "taiyuan-login".into(),
        address: "tycs.example.com".into(),
        platform: "Linux".into(),
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

    async fn terminal_command(&self, profile: &SshProfile) -> Result<SshLaunchCommand> {
        Ok(SshLaunchCommand {
            profile_id: profile.id.clone(),
            program: "/mock/ssh".into(),
            args: vec!["--".into(), profile.host.clone()],
            env: HashMap::new(),
        })
    }

    async fn report_terminal_launch_failure(&self, _executable: &str) {}

    async fn test_connection(&self, _profile: &SshProfile) -> Result<()> {
        Ok(())
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
fn production_profile_allows_terminal_but_blocks_sftp_writes() {
    let mut profile = SshProfile::new("production", "server.example");
    profile.production = true;
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage));

    let command = futures::executor::block_on(service.terminal_command(&profile)).unwrap();
    assert_eq!(command.program, "/mock/ssh");

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
        futures::executor::block_on(service.execute_transfer(
            &queued,
            &profile,
            OverwritePolicy::Refuse
        )),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
}

#[test]
fn remote_editor_rejects_content_above_preview_bound() {
    let profile = SshProfile::new("server", "server.example");
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage));
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
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage));
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
    });
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage))
        .with_jumpserver_driver(jumpserver);
    let session = jumpserver_session();
    let asset = jumpserver_asset();

    let tested =
        futures::executor::block_on(service.test_jumpserver_asset(&session, &asset, "account-1"))
            .unwrap();
    let saved =
        futures::executor::block_on(service.save_jumpserver_asset(&session, &asset, "account-1"))
            .unwrap();

    assert_eq!(detail_calls.load(Ordering::SeqCst), 2);
    for profile in [tested, saved] {
        assert_eq!(profile.host, "jump.example.com");
        assert_eq!(profile.port, Some(2222));
        assert_eq!(
            profile.username,
            "alice#root#00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(profile.auth_mode, SshAuthMode::Password);
        assert_eq!(profile.password, "login-password");
    }
}

#[test]
fn jumpserver_profile_uses_connect_permission_not_managed_secret_flag() {
    let mut detail = JumpServerAssetDetail {
        asset: jumpserver_asset(),
        accounts: vec![JumpServerAccount {
            id: "account-1".into(),
            name: "root".into(),
            username: "root".into(),
            has_secret: false,
            can_connect: true,
        }],
        ssh_enabled: true,
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
