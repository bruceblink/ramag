//! 有界并发的流式上传/下载与临时文件提交。

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

use ramag_domain::entities::{
    MAX_CONCURRENT_TRANSFERS, OverwritePolicy, SshProgressFn, TransferCancellation,
};
use ramag_domain::error::{DomainError, Result};

use crate::session::{StructuredSftpSession, map_sftp_error};

mod archive;
mod commit;

#[cfg(test)]
use commit::commit_local_blocking;
use commit::{
    cleanup_local, cleanup_remote, commit_local, commit_remote, local_sibling, remote_sibling,
};

#[derive(Clone)]
pub struct TransferEngine {
    semaphore: Arc<Semaphore>,
}

impl Default for TransferEngine {
    fn default() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS)),
        }
    }
}

impl TransferEngine {
    #[allow(clippy::too_many_arguments)]
    pub async fn download_directory(
        &self,
        session: Arc<StructuredSftpSession>,
        remote_path: String,
        local_path: PathBuf,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()> {
        let _permit = acquire_permit(self.semaphore.clone(), &cancellation).await?;
        archive::download_directory(
            session,
            remote_path,
            local_path,
            overwrite,
            cancellation,
            progress,
        )
        .await
    }

    pub async fn save_file(
        &self,
        session: Arc<StructuredSftpSession>,
        remote_path: String,
        expected: Vec<u8>,
        contents: Vec<u8>,
    ) -> Result<()> {
        let _permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DomainError::Other("SSH 传输调度器已关闭".into()))?;
        let metadata = session
            .raw
            .lstat(remote_path.clone())
            .await
            .map_err(|error| map_sftp_error("读取远程编辑文件信息", error))?
            .attrs;
        if !metadata.is_regular() {
            return Err(DomainError::Forbidden(
                "仅支持保存普通文件，不跟随符号链接".into(),
            ));
        }
        ensure_remote_contents_match(&session, &remote_path, &expected).await?;

        let temporary = remote_sibling(&remote_path, "ramag-edit")?;
        let mut attributes = FileAttributes::empty();
        attributes.permissions = metadata.permissions;
        let handle = session
            .raw
            .open(
                temporary.clone(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                attributes,
            )
            .await
            .map_err(|error| map_sftp_error("创建远程编辑临时文件", error))?
            .handle;
        let write_result = async {
            let mut offset = 0u64;
            for chunk in contents.chunks(session.write_chunk_bytes) {
                session
                    .raw
                    .write(handle.clone(), offset, chunk.to_vec())
                    .await
                    .map_err(|error| map_sftp_error("写入远程编辑临时文件", error))?;
                offset = offset.saturating_add(chunk.len() as u64);
            }
            if metadata.permissions.is_some() {
                let mut preserved = FileAttributes::empty();
                preserved.permissions = metadata.permissions;
                session
                    .raw
                    .fsetstat(handle.clone(), preserved)
                    .await
                    .map_err(|error| map_sftp_error("保留远程文件权限", error))?;
            }
            if session.supports_fsync {
                session
                    .raw
                    .fsync(handle.clone())
                    .await
                    .map_err(|error| map_sftp_error("同步远程编辑临时文件", error))?;
            }
            Ok(())
        }
        .await;
        let close_result = session
            .raw
            .close(handle)
            .await
            .map(|_| ())
            .map_err(|error| map_sftp_error("关闭远程编辑临时文件", error));
        let result = match (write_result, close_result) {
            (Ok(()), close) => close,
            (Err(_), Err(error @ DomainError::ConnectionFailed(_))) => Err(error),
            (Err(error), _) => Err(error),
        };
        if let Err(error) = result {
            cleanup_remote(&session, &temporary).await;
            return Err(error);
        }

        if let Err(error) = ensure_remote_contents_match(&session, &remote_path, &expected).await {
            cleanup_remote(&session, &temporary).await;
            return Err(error);
        }
        if let Err(error) = commit_remote(&session, &temporary, &remote_path, true).await {
            cleanup_remote(&session, &temporary).await;
            return Err(error);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upload(
        &self,
        session: Arc<StructuredSftpSession>,
        local_path: PathBuf,
        remote_path: String,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()> {
        let _permit = acquire_permit(self.semaphore.clone(), &cancellation).await?;
        ensure_not_cancelled(&cancellation)?;
        let metadata = tokio::fs::symlink_metadata(&local_path)
            .await
            .map_err(|error| DomainError::Other(format!("读取本地上传文件信息失败：{error}")))?;
        if !metadata.is_file() {
            return Err(DomainError::InvalidConfig(
                "上传源必须是普通文件；首个版本不上传目录或符号链接".into(),
            ));
        }
        let mut source = File::open(&local_path)
            .await
            .map_err(|error| DomainError::Other(format!("打开本地上传文件失败：{error}")))?;
        let opened_metadata = source
            .metadata()
            .await
            .map_err(|error| DomainError::Other(format!("读取已打开上传文件信息失败：{error}")))?;
        if !opened_metadata.is_file() {
            return Err(DomainError::InvalidConfig("上传源不再是普通文件".into()));
        }
        let total = opened_metadata.len();
        progress(0, total);

        let existing = remote_lstat(&session, &remote_path, "检查远程目标", &cancellation).await?;
        let existed = existing.is_some();
        if existed && overwrite == OverwritePolicy::Refuse {
            return Err(DomainError::Forbidden(
                "远程目标已存在；请明确确认覆盖后重试".into(),
            ));
        }
        if existing.is_some_and(|attributes| !attributes.is_regular()) {
            return Err(DomainError::Forbidden(
                "只允许覆盖远程普通文件，不能覆盖目录、符号链接或特殊文件".into(),
            ));
        }
        let temporary = remote_sibling(&remote_path, "ramag-upload")?;
        let handle = await_cancellable_sftp(
            session.raw.open(
                temporary.clone(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes::empty(),
            ),
            &cancellation,
        )
        .await?
        .map_err(|error| map_sftp_error("创建远程上传临时文件", error))?
        .handle;
        let mut transfer_result = copy_upload(
            &session,
            &mut source,
            &handle,
            total,
            &cancellation,
            &progress,
        )
        .await;
        if let Ok(transferred) = transfer_result.as_ref().copied()
            && session.supports_fsync
        {
            transfer_result =
                await_cancellable_sftp(session.raw.fsync(handle.clone()), &cancellation)
                    .await
                    .and_then(|result| {
                        result.map_err(|error| map_sftp_error("同步远程上传临时文件", error))
                    })
                    .map(|_| transferred);
        }
        let close_result = session
            .raw
            .close(handle)
            .await
            .map(|_| ())
            .map_err(|error| map_sftp_error("关闭远程上传临时文件", error));
        if let Err(error) = close_result {
            if transfer_result.is_ok() || matches!(error, DomainError::ConnectionFailed(_)) {
                transfer_result = Err(error);
            } else {
                tracing::warn!(error = %error, "close failed ssh upload handle failed");
            }
        }
        let transferred = match transfer_result {
            Ok(transferred) => transferred,
            Err(error) => {
                cleanup_remote(&session, &temporary).await;
                return Err(error);
            }
        };
        if transferred != total {
            cleanup_remote(&session, &temporary).await;
            return Err(changed_upload_size(total, transferred));
        }
        if let Err(error) = ensure_not_cancelled(&cancellation) {
            cleanup_remote(&session, &temporary).await;
            return Err(error);
        }

        if let Err(error) = commit_remote(&session, &temporary, &remote_path, existed).await {
            cleanup_remote(&session, &temporary).await;
            return Err(error);
        }
        progress(total, total);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download(
        &self,
        session: Arc<StructuredSftpSession>,
        remote_path: String,
        local_path: PathBuf,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: SshProgressFn,
    ) -> Result<()> {
        let _permit = acquire_permit(self.semaphore.clone(), &cancellation).await?;
        ensure_not_cancelled(&cancellation)?;
        let metadata =
            await_cancellable_sftp(session.raw.lstat(remote_path.clone()), &cancellation)
                .await?
                .map_err(|error| map_sftp_error("读取远程下载文件信息", error))?
                .attrs;
        if !metadata.is_regular() {
            return Err(DomainError::InvalidConfig(
                "下载源必须是普通文件；首个版本不下载目录或符号链接".into(),
            ));
        }
        let target_exists = tokio::fs::try_exists(&local_path)
            .await
            .map_err(|error| DomainError::Other(format!("检查本地下载目标失败：{error}")))?;
        if target_exists && overwrite == OverwritePolicy::Refuse {
            return Err(DomainError::Forbidden(
                "本地目标已存在；请明确确认覆盖后重试".into(),
            ));
        }
        let temporary = local_sibling(&local_path)?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| DomainError::Other(format!("创建本地下载临时文件失败：{error}")))?;
        let handle = match await_cancellable_sftp(
            session
                .raw
                .open(remote_path, OpenFlags::READ, FileAttributes::empty()),
            &cancellation,
        )
        .await?
        {
            Ok(handle) => handle.handle,
            Err(error) => {
                drop(destination);
                cleanup_local(&temporary).await;
                return Err(map_sftp_error("打开远程下载文件", error));
            }
        };
        let opened_metadata =
            match await_cancellable_sftp(session.raw.fstat(handle.clone()), &cancellation).await? {
                Ok(metadata) if metadata.attrs.is_regular() => metadata.attrs,
                Ok(_) => {
                    let _ = session.raw.close(handle).await;
                    drop(destination);
                    cleanup_local(&temporary).await;
                    return Err(DomainError::Forbidden(
                        "远程下载源在打开时已不再是普通文件".into(),
                    ));
                }
                Err(error) => {
                    let _ = session.raw.close(handle).await;
                    drop(destination);
                    cleanup_local(&temporary).await;
                    return Err(map_sftp_error("确认已打开远程下载文件", error));
                }
            };
        let Some(total) = opened_metadata.size else {
            let _ = session.raw.close(handle).await;
            drop(destination);
            cleanup_local(&temporary).await;
            return Err(DomainError::Other(
                "远端未返回下载文件大小，无法执行有界传输".into(),
            ));
        };
        progress(0, total);
        let mut transfer_result = copy_download(
            &session,
            &handle,
            &mut destination,
            total,
            &cancellation,
            &progress,
        )
        .await;
        let close_result = session
            .raw
            .close(handle)
            .await
            .map(|_| ())
            .map_err(|error| map_sftp_error("关闭远程下载文件", error));
        if let Err(error) = close_result {
            if transfer_result.is_ok() || matches!(error, DomainError::ConnectionFailed(_)) {
                transfer_result = Err(error);
            } else {
                tracing::warn!(error = %error, "close failed ssh download handle failed");
            }
        }
        let transferred = match transfer_result {
            Ok(transferred) => transferred,
            Err(error) => {
                drop(destination);
                cleanup_local(&temporary).await;
                return Err(error);
            }
        };
        if transferred != total {
            drop(destination);
            cleanup_local(&temporary).await;
            return Err(DomainError::ConnectionFailed(format!(
                "下载期间远程文件大小发生变化：开始时 {total} bytes，实际读取 {transferred} bytes"
            )));
        }
        let finalize = async {
            destination.flush().await.map_err(|error| {
                DomainError::Other(format!("刷新本地下载临时文件失败：{error}"))
            })?;
            destination
                .sync_all()
                .await
                .map_err(|error| DomainError::Other(format!("同步本地下载临时文件失败：{error}")))
        }
        .await;
        if let Err(error) = finalize {
            drop(destination);
            cleanup_local(&temporary).await;
            return Err(error);
        }
        drop(destination);
        if let Err(error) = ensure_not_cancelled(&cancellation) {
            cleanup_local(&temporary).await;
            return Err(error);
        }
        if let Err(error) = commit_local(&temporary, &local_path, overwrite).await {
            cleanup_local(&temporary).await;
            return Err(error);
        }
        progress(total, total);
        Ok(())
    }
}

async fn ensure_remote_contents_match(
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

async fn copy_upload(
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

async fn copy_download(
    session: &StructuredSftpSession,
    handle: &str,
    destination: &mut File,
    total: u64,
    cancellation: &TransferCancellation,
    progress: &SshProgressFn,
) -> Result<u64> {
    let mut transferred = 0u64;
    loop {
        ensure_not_cancelled(cancellation)?;
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

fn changed_upload_size(expected: u64, actual: u64) -> DomainError {
    DomainError::Other(format!(
        "上传期间本地文件大小发生变化：开始时 {expected} bytes，实际读取 {actual} bytes"
    ))
}

async fn await_cancellable_sftp<F, T>(
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

async fn acquire_permit(
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

fn ensure_not_cancelled(cancellation: &TransferCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(DomainError::Other("传输已取消".into()))
    } else {
        Ok(())
    }
}

async fn remote_lstat(
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

#[cfg(test)]
mod tests;
