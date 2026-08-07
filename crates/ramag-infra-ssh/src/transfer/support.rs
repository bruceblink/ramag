use super::*;

pub(super) async fn ensure_remote_contents_match(
    session: &StructuredSftpSession,
    path: &str,
    expected: &[u8],
) -> Result<()> {
    let current = crate::session::read_file_preview(session, path).await?;
    if current.truncated || current.bytes != expected {
        return Err(DomainError::Forbidden(
            "远程文件已变化，请重新打开后再保存".into(),
        ));
    }
    Ok(())
}

pub(super) async fn copy_upload(
    session: &StructuredSftpSession,
    source: &mut File,
    handle: &str,
    total: u64,
    cancellation: &TransferCancellation,
    progress: &SshProgressFn,
) -> Result<u64> {
    let mut buffer = vec![0u8; session.write_chunk_bytes];
    let mut transferred = 0u64;
    loop {
        ensure_not_cancelled(cancellation)?;
        let read = source
            .read(&mut buffer)
            .await
            .map_err(|error| DomainError::Other(format!("读取本地上传文件失败：{error}")))?;
        if read == 0 {
            break;
        }
        let next = transferred.saturating_add(read as u64);
        if next > total {
            return Err(changed_upload_size(total, next));
        }
        await_cancellable_sftp(
            session
                .raw
                .write(handle.to_string(), transferred, buffer[..read].to_vec()),
            cancellation,
        )
        .await?
        .map_err(|error| map_sftp_error("写入远程上传文件", error))?;
        transferred = next;
        progress(transferred, total);
    }
    Ok(transferred)
}

pub(super) async fn copy_download(
    session: &StructuredSftpSession,
    handle: &str,
    destination: &mut File,
    total: u64,
    cancellation: &TransferCancellation,
    progress: &SshProgressFn,
    deadline: Option<Instant>,
) -> Result<u64> {
    let mut transferred = 0u64;
    loop {
        ensure_not_cancelled(cancellation)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(DomainError::Forbidden(format!(
                "生产下载超过 {MAX_PRODUCTION_DOWNLOAD_SECONDS} 秒安全上限"
            )));
        }
        let data = match await_cancellable_sftp(
            session
                .raw
                .read(handle.to_string(), transferred, session.read_chunk_bytes),
            cancellation,
        )
        .await?
        {
            Ok(data) => data.data,
            Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => break,
            Err(error) => return Err(map_sftp_error("读取远程下载文件", error)),
        };
        if data.is_empty() {
            return Err(DomainError::ConnectionFailed(
                "远端返回了空下载数据包而非结束标记".into(),
            ));
        }
        let next = transferred.saturating_add(data.len() as u64);
        if next > total {
            return Err(DomainError::ConnectionFailed(format!(
                "下载期间远程文件大小发生变化：开始时 {total} bytes，已收到 {next} bytes"
            )));
        }
        destination
            .write_all(&data)
            .await
            .map_err(|error| DomainError::Other(format!("写入本地下载文件失败：{error}")))?;
        transferred = next;
        progress(transferred, total);
    }
    Ok(transferred)
}

pub(super) fn ensure_production_download_size(size: Option<u64>) -> Result<()> {
    let size =
        size.ok_or_else(|| DomainError::Forbidden("生产下载必须先获得远程文件大小".into()))?;
    if size > MAX_PRODUCTION_DOWNLOAD_BYTES {
        return Err(DomainError::Forbidden(format!(
            "生产下载文件超过 {} MiB 安全上限",
            MAX_PRODUCTION_DOWNLOAD_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

pub(super) fn changed_upload_size(expected: u64, actual: u64) -> DomainError {
    DomainError::Other(format!(
        "上传期间本地文件大小发生变化：开始时 {expected} bytes，实际读取 {actual} bytes"
    ))
}

pub(super) async fn await_cancellable_sftp<F, T>(
    future: F,
    cancellation: &TransferCancellation,
) -> Result<std::result::Result<T, SftpError>>
where
    F: Future<Output = std::result::Result<T, SftpError>>,
{
    tokio::pin!(future);
    loop {
        ensure_not_cancelled(cancellation)?;
        match timeout(Duration::from_millis(100), &mut future).await {
            Ok(result) => return Ok(result),
            Err(_) => continue,
        }
    }
}

pub(super) async fn acquire_permit(
    semaphore: Arc<Semaphore>,
    cancellation: &TransferCancellation,
) -> Result<OwnedSemaphorePermit> {
    loop {
        ensure_not_cancelled(cancellation)?;
        match timeout(
            Duration::from_millis(100),
            semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => return Ok(permit),
            Ok(Err(_)) => return Err(DomainError::Other("SSH 传输调度器已关闭".into())),
            Err(_) => {}
        }
    }
}

pub(super) fn ensure_not_cancelled(cancellation: &TransferCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(DomainError::Other("传输已取消".into()))
    } else {
        Ok(())
    }
}

pub(super) async fn remote_lstat(
    session: &StructuredSftpSession,
    path: &str,
    context: &str,
    cancellation: &TransferCancellation,
) -> Result<Option<FileAttributes>> {
    match await_cancellable_sftp(session.raw.lstat(path.to_string()), cancellation).await? {
        Ok(metadata) => Ok(Some(metadata.attrs)),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(map_sftp_error(context, error)),
    }
}
