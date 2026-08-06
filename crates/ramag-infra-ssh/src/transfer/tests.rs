use super::*;

#[test]
fn temporary_paths_stay_next_to_target() {
    let remote = remote_sibling("/home/alice/a.txt", "ramag-upload").unwrap();
    assert!(remote.starts_with("/home/alice/.a.txt.ramag-upload-"));
    assert!(remote.ends_with(".tmp"));

    let directory = tempfile::tempdir().unwrap();
    let local = local_sibling(&directory.path().join("a.txt")).unwrap();
    assert_eq!(local.parent(), Some(directory.path()));
}

#[test]
fn refuse_commit_never_overwrites_existing_local_file() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = directory.path().join("temporary");
    let target = directory.path().join("target");
    std::fs::write(&temporary, b"new").unwrap();
    std::fs::write(&target, b"old").unwrap();

    let result = commit_local_blocking(&temporary, &target, OverwritePolicy::Refuse);
    assert!(matches!(result, Err(DomainError::Forbidden(_))));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(std::fs::read(&temporary).unwrap(), b"new");
}

#[test]
fn cancelled_transfer_is_explicit() {
    let cancellation = TransferCancellation::default();
    cancellation.cancel();
    assert!(
        ensure_not_cancelled(&cancellation)
            .unwrap_err()
            .message()
            .contains("已取消")
    );
}

#[test]
fn production_download_size_requires_known_bounded_size() {
    assert!(ensure_production_download_size(None).is_err());
    assert!(ensure_production_download_size(Some(MAX_PRODUCTION_DOWNLOAD_BYTES)).is_ok());
    assert!(ensure_production_download_size(Some(MAX_PRODUCTION_DOWNLOAD_BYTES + 1)).is_err());
}

#[test]
fn production_download_concurrency_is_rejected_without_waiting() {
    let engine = TransferEngine::default();
    let permit = engine.production_download_permit(true).unwrap();
    assert!(permit.is_some());
    assert!(engine.production_download_permit(true).is_err());
    drop(permit);
    assert!(engine.production_download_permit(true).is_ok());
}
