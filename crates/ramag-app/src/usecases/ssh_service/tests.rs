use super::*;
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, QueryRecord, QueryRecordId, SshProgressFn, SshWorkspaceState,
};
use ramag_domain::error::READ_ONLY_MESSAGE;

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
    })
    .unwrap();

    let parsed = parse_workspace_preference(&json).unwrap();
    assert_eq!(parsed.workspaces.len(), 1);
    assert!(parsed.active_profile_id.is_none());
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

    assert!(matches!(
        futures::executor::block_on(service.create_directory(&profile, "/new-directory")),
        Err(DomainError::Forbidden(message)) if message == READ_ONLY_MESSAGE
    ));
}
