//! 远程目录的有界扫描与流式 tar.gz 下载。

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_compression::futures::write::GzipEncoder;
use async_tar::{Builder, EntryType, Header};
use futures::io::AsyncWriteExt as _;
use futures::{StreamExt as _, TryStreamExt as _};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};

use ramag_domain::entities::{
    MAX_PRODUCTION_DIRECTORY_ENTRIES, MAX_PRODUCTION_DOWNLOAD_BYTES,
    MAX_PRODUCTION_DOWNLOAD_SECONDS, MAX_REMOTE_ARCHIVE_DEPTH, MAX_REMOTE_ARCHIVE_ENTRIES,
    MAX_REMOTE_ARCHIVE_RETAINED_BYTES, MAX_SSH_PATH_BYTES, OverwritePolicy, RemotePath,
    SftpNamespaceKind, SshProgressFn, TransferCancellation, infer_sftp_namespace, join_remote_path,
    validate_remote_name, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};

use crate::session::{StructuredSftpSession, map_sftp_error, read_directory_files};

use super::commit::{cleanup_local, commit_local, local_sibling};
use super::{await_cancellable_sftp, ensure_not_cancelled};

mod scan;

enum ArchiveKind {
    Directory,
    File,
    Symlink(String),
}

struct ArchiveEntry {
    remote_path: String,
    archive_path: async_std::path::PathBuf,
    attributes: FileAttributes,
    kind: ArchiveKind,
}

struct ArchivePlan {
    entries: Vec<ArchiveEntry>,
    total_bytes: u64,
}

struct RemoteReadState {
    session: Arc<StructuredSftpSession>,
    handle: String,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
    offset: u64,
    size: u64,
    completed_before: u64,
    archive_total: u64,
    deadline: Option<std::time::Instant>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn download_directory(
    session: Arc<StructuredSftpSession>,
    remote_path: String,
    local_path: PathBuf,
    overwrite: OverwritePolicy,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
    production: bool,
) -> Result<()> {
    ensure_not_cancelled(&cancellation)?;
    let target_exists = tokio::fs::try_exists(&local_path)
        .await
        .map_err(|error| DomainError::Other(format!("检查本地目录下载目标失败：{error}")))?;
    if target_exists && overwrite == OverwritePolicy::Refuse {
        return Err(DomainError::Forbidden(
            "本地目标已存在；请明确确认覆盖后重试".into(),
        ));
    }
    let deadline = production.then(|| {
        std::time::Instant::now() + std::time::Duration::from_secs(MAX_PRODUCTION_DOWNLOAD_SECONDS)
    });
    let plan =
        scan::scan_directory(&session, &remote_path, &cancellation, production, deadline).await?;
    progress(0, plan.total_bytes);
    let temporary = local_sibling(&local_path)?;
    let result = write_archive(
        session,
        &temporary,
        plan,
        cancellation.clone(),
        progress.clone(),
        deadline,
    )
    .await;
    if let Err(error) = result {
        cleanup_local(&temporary).await;
        return Err(error);
    }
    if let Err(error) = ensure_not_cancelled(&cancellation) {
        cleanup_local(&temporary).await;
        return Err(error);
    }
    if let Err(error) = commit_local(&temporary, &local_path, overwrite).await {
        cleanup_local(&temporary).await;
        return Err(error);
    }
    Ok(())
}

async fn write_archive(
    session: Arc<StructuredSftpSession>,
    temporary: &Path,
    plan: ArchivePlan,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
    deadline: Option<std::time::Instant>,
) -> Result<()> {
    let destination = async_std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .await
        .map_err(|error| DomainError::Other(format!("创建本地目录下载临时文件失败：{error}")))?;
    let encoder = GzipEncoder::new(destination);
    let mut archive = Builder::new(encoder);
    let mut completed = 0u64;
    for entry in plan.entries {
        ensure_archive_deadline(deadline)?;
        ensure_not_cancelled(&cancellation)?;
        let mut header = archive_header(&entry);
        match &entry.kind {
            ArchiveKind::Directory => {
                archive
                    .append_data(&mut header, &entry.archive_path, futures::io::empty())
                    .await
                    .map_err(|error| map_archive_io("写入目录归档", error))?;
            }
            ArchiveKind::Symlink(target) => {
                header
                    .set_link_name(target)
                    .map_err(|error| map_archive_io("写入符号链接", error))?;
                archive
                    .append_data(&mut header, &entry.archive_path, futures::io::empty())
                    .await
                    .map_err(|error| map_archive_io("写入符号链接", error))?;
            }
            ArchiveKind::File => {
                append_remote_file(
                    &mut archive,
                    &mut header,
                    &entry,
                    session.clone(),
                    cancellation.clone(),
                    progress.clone(),
                    completed,
                    plan.total_bytes,
                    deadline,
                )
                .await?;
                completed = completed.saturating_add(entry.attributes.len());
            }
        }
    }
    let mut encoder = archive
        .into_inner()
        .await
        .map_err(|error| map_archive_io("结束目录归档", error))?;
    encoder
        .close()
        .await
        .map_err(|error| map_archive_io("结束目录压缩", error))?;
    let destination = encoder.into_inner();
    destination
        .sync_all()
        .await
        .map_err(|error| DomainError::Other(format!("同步本地目录下载临时文件失败：{error}")))?;
    progress(plan.total_bytes, plan.total_bytes);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_remote_file<W>(
    archive: &mut Builder<W>,
    header: &mut Header,
    entry: &ArchiveEntry,
    session: Arc<StructuredSftpSession>,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
    completed_before: u64,
    archive_total: u64,
    deadline: Option<std::time::Instant>,
) -> Result<()>
where
    W: async_std::io::Write + Unpin + Send + Sync,
{
    let handle = await_cancellable_sftp(
        session.raw.open(
            entry.remote_path.clone(),
            OpenFlags::READ,
            FileAttributes::empty(),
        ),
        &cancellation,
    )
    .await?
    .map_err(|error| map_sftp_error("打开远程归档文件", error))?
    .handle;
    let opened = match await_cancellable_sftp(session.raw.fstat(handle.clone()), &cancellation)
        .await
    {
        Ok(Ok(metadata)) => metadata.attrs,
        Ok(Err(error)) => {
            if let Err(close_error) = session.raw.close(handle).await {
                tracing::warn!(
                    operation = "ssh_archive_file_append",
                    stage = "metadata_close",
                    error = %close_error,
                    "close ssh archive source after metadata failure failed"
                );
            }
            return Err(map_sftp_error("确认远程归档文件", error));
        }
        Err(error) => {
            if let Err(close_error) = session.raw.close(handle).await {
                tracing::warn!(operation = "ssh_archive_file_append", stage = "cancelled", error = %close_error, "close cancelled ssh archive source failed");
            }
            return Err(error);
        }
    };
    if !opened.is_regular() || opened.len() != entry.attributes.len() {
        if let Err(error) = session.raw.close(handle).await {
            tracing::warn!(operation = "ssh_archive_file_append", stage = "changed_source", error = %error, "close changed ssh archive source failed");
        }
        return Err(DomainError::Forbidden("目录内容已变化，请重新下载".into()));
    }
    let stream = futures::stream::try_unfold(
        RemoteReadState {
            session: session.clone(),
            handle: handle.clone(),
            cancellation,
            progress,
            offset: 0,
            size: opened.len(),
            completed_before,
            archive_total,
            deadline,
        },
        |mut state| async move {
            if state.offset == state.size {
                return Ok(None);
            }
            ensure_archive_deadline(state.deadline).map_err(domain_to_io)?;
            ensure_not_cancelled(&state.cancellation).map_err(domain_to_io)?;
            let remaining = state.size - state.offset;
            let request = remaining.min(u64::from(state.session.read_chunk_bytes)) as u32;
            let data = match state
                .session
                .raw
                .read(state.handle.clone(), state.offset, request)
                .await
            {
                Ok(data) => data.data,
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "目录下载期间远程文件缩短",
                    ));
                }
                Err(error) => {
                    return Err(domain_to_io(map_sftp_error("读取远程归档文件", error)));
                }
            };
            if data.is_empty() || data.len() as u64 > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "远端返回了无效归档数据",
                ));
            }
            state.offset += data.len() as u64;
            (state.progress)(
                state.completed_before.saturating_add(state.offset),
                state.archive_total,
            );
            Ok(Some((data, state)))
        },
    )
    .boxed()
    .into_async_read();
    let result = archive
        .append_data(header, &entry.archive_path, stream)
        .await
        .map_err(|error| map_archive_io("写入远程文件归档", error));
    let close_result = session
        .raw
        .close(handle)
        .await
        .map(|_| ())
        .map_err(|error| map_sftp_error("关闭远程归档文件", error));
    match (result, close_result) {
        (Ok(()), close) => close,
        (Err(_), Err(error @ DomainError::ConnectionFailed(_))) => Err(error),
        (Err(error), _) => Err(error),
    }
}

fn ensure_archive_deadline(deadline: Option<std::time::Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return Err(DomainError::Other(format!(
            "生产目录下载超过 {MAX_PRODUCTION_DOWNLOAD_SECONDS} 秒硬超时"
        )));
    }
    Ok(())
}

fn archive_header(entry: &ArchiveEntry) -> Header {
    let mut header = Header::new_gnu();
    let (kind, size, default_mode) = match &entry.kind {
        ArchiveKind::Directory => (EntryType::Directory, 0, 0o755),
        ArchiveKind::File => (EntryType::Regular, entry.attributes.len(), 0o644),
        ArchiveKind::Symlink(_) => (EntryType::Symlink, 0, 0o777),
    };
    header.set_entry_type(kind);
    header.set_size(size);
    header.set_mode(entry.attributes.permissions.unwrap_or(default_mode) & 0o7777);
    header.set_uid(u64::from(entry.attributes.uid.unwrap_or(0)));
    header.set_gid(u64::from(entry.attributes.gid.unwrap_or(0)));
    header.set_mtime(u64::from(entry.attributes.mtime.unwrap_or(0)));
    header
}

fn archive_attributes(attributes: &FileAttributes) -> FileAttributes {
    FileAttributes {
        size: attributes.size,
        uid: attributes.uid,
        user: None,
        gid: attributes.gid,
        group: None,
        permissions: attributes.permissions,
        atime: None,
        mtime: attributes.mtime,
    }
}

fn charge_path_bytes(
    retained: &mut usize,
    remote: &str,
    archive: &async_std::path::Path,
    limit: usize,
) -> Result<()> {
    let additional = remote
        .len()
        .saturating_mul(2)
        .saturating_add(archive.as_os_str().to_string_lossy().len())
        .saturating_add(96);
    charge_retained_bytes(retained, additional, limit)
}

fn charge_retained_bytes(retained: &mut usize, additional: usize, limit: usize) -> Result<()> {
    *retained = retained
        .checked_add(additional)
        .ok_or_else(|| DomainError::Forbidden("目录路径数据大小溢出".into()))?;
    if *retained > limit {
        return Err(DomainError::Forbidden(format!(
            "目录路径数据超过 {} MiB",
            limit / 1024 / 1024
        )));
    }
    Ok(())
}

fn archive_entry_limit(max_entries: usize) -> DomainError {
    DomainError::Forbidden(format!("目录项目超过 {max_entries} 个"))
}

fn domain_to_io(error: DomainError) -> io::Error {
    let kind = if matches!(error, DomainError::ConnectionFailed(_)) {
        io::ErrorKind::ConnectionAborted
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, error)
}

fn map_archive_io(context: &str, error: io::Error) -> DomainError {
    if error.kind() == io::ErrorKind::ConnectionAborted {
        DomainError::ConnectionFailed(format!("{context}失败：{error}"))
    } else {
        DomainError::Other(format!("{context}失败：{error}"))
    }
}

#[cfg(test)]
#[path = "archive/tests.rs"]
mod tests;
