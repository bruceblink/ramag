use super::*;
use ramag_domain::entities::SshWorkspaceState;

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
