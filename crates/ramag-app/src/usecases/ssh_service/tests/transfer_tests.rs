use super::*;

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
fn production_profile_allows_terminal_and_blocks_sftp_writes() {
    let mut profile = SshProfile::new("production", "server.example");
    profile.production = true;
    let service = SshService::new(Arc::new(TerminalDriver), Arc::new(NoopStorage::default()));

    futures::executor::block_on(service.save_profile(&profile)).unwrap();
    let command = futures::executor::block_on(service.terminal_command(&profile.id, None)).unwrap();
    assert!(service.terminal_launch_is_current(&command));

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
