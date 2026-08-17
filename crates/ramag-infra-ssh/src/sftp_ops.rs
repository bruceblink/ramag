//! OpenSSH 驱动的 SFTP 文件与传输操作。

use super::*;

pub(super) async fn list_directory(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    path: &str,
) -> Result<RemoteDirectory> {
    profile.validate().map_err(DomainError::InvalidConfig)?;
    validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
    let profile = profile.clone();
    let path = path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
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

pub(super) async fn read_file_preview(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    path: &str,
) -> Result<RemoteFilePreview> {
    validate_profile_and_path(profile, path)?;
    let profile = profile.clone();
    let path = path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_file_preview",
            session::read_file_preview(&connection.session, &path).await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn read_file_chunk(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    path: &str,
    position: RemoteFileChunkPosition,
) -> Result<RemoteFileChunk> {
    validate_profile_and_path(profile, path)?;
    let profile = profile.clone();
    let path = path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_file_chunk",
            session::read_file_chunk(&connection.session, &path, position).await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn save_file(
    driver: &OpenSshDriver,
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
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    let transfers = driver.transfers.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_file_save",
            transfers
                .save_file(connection.session.clone(), path, expected, contents)
                .await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn create_directory(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    path: &str,
) -> Result<()> {
    validate_writable_profile_and_path(profile, path)?;
    let profile = profile.clone();
    let path = path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_directory_create",
            session::create_directory(&connection.session, &path).await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn rename(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    old_path: &str,
    new_path: &str,
) -> Result<()> {
    validate_writable_profile_and_path(profile, old_path)?;
    validate_remote_path(new_path).map_err(DomainError::InvalidConfig)?;
    let profile = profile.clone();
    let old_path = old_path.to_string();
    let new_path = new_path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_entry_rename",
            session::rename(&connection.session, &old_path, &new_path).await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn remove(
    driver: &OpenSshDriver,
    profile: &SshProfile,
    path: &str,
    kind: RemoteEntryKind,
) -> Result<()> {
    validate_writable_profile_and_path(profile, path)?;
    let profile = profile.clone();
    let path = path.to_string();
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_entry_remove",
            session::remove(&connection.session, &path, kind).await,
        );
        invalidate_broken(&sessions, &profile.id, &result).await;
        result
    })
    .await
}

pub(super) async fn upload(
    driver: &OpenSshDriver,
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
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    let transfers = driver.transfers.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_upload",
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

pub(super) async fn download(
    driver: &OpenSshDriver,
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
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    let transfers = driver.transfers.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_download",
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

pub(super) async fn download_directory(
    driver: &OpenSshDriver,
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
    let locator = driver.locator.clone();
    let sessions = driver.sessions.clone();
    let transfers = driver.transfers.clone();
    run_in_tokio(async move {
        let connection = connect(&locator, &sessions, &profile).await?;
        let result = connection.contextualize(
            "ssh_sftp_directory_download",
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
