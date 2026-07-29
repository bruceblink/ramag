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
    MAX_REMOTE_ARCHIVE_DEPTH, MAX_REMOTE_ARCHIVE_ENTRIES, MAX_REMOTE_ARCHIVE_RETAINED_BYTES,
    MAX_SSH_PATH_BYTES, OverwritePolicy, SshProgressFn, TransferCancellation, join_remote_path,
    validate_remote_name, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};

use crate::session::{StructuredSftpSession, map_sftp_error, read_directory_files};

use super::commit::{cleanup_local, commit_local, local_sibling};
use super::{await_cancellable_sftp, ensure_not_cancelled};

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
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn download_directory(
    session: Arc<StructuredSftpSession>,
    remote_path: String,
    local_path: PathBuf,
    overwrite: OverwritePolicy,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
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
    let plan = scan_directory(&session, &remote_path, &cancellation).await?;
    progress(0, plan.total_bytes);
    let temporary = local_sibling(&local_path)?;
    let result = write_archive(
        session,
        &temporary,
        plan,
        cancellation.clone(),
        progress.clone(),
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

async fn scan_directory(
    session: &StructuredSftpSession,
    remote_path: &str,
    cancellation: &TransferCancellation,
) -> Result<ArchivePlan> {
    validate_remote_path(remote_path).map_err(DomainError::InvalidConfig)?;
    let canonical = session
        .canonicalize(remote_path.to_string())
        .await
        .map_err(|error| map_sftp_error("解析远程下载目录", error))?;
    validate_remote_path(&canonical)
        .map_err(|error| DomainError::Other(format!("远端返回了无效目录路径：{error}")))?;
    let root_attributes = session
        .raw
        .lstat(canonical.clone())
        .await
        .map_err(|error| map_sftp_error("读取远程下载目录信息", error))?
        .attrs;
    if !root_attributes.is_dir() {
        return Err(DomainError::InvalidConfig("下载源必须是目录".into()));
    }
    let root_name = canonical
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("root");
    validate_remote_name(root_name).map_err(DomainError::InvalidConfig)?;

    let root_archive = async_std::path::PathBuf::from(root_name);
    let mut queue = VecDeque::from([(canonical.clone(), root_archive.clone(), 0usize)]);
    let mut seen = HashSet::from([canonical]);
    let mut entries = Vec::new();
    let mut retained_bytes = 0usize;
    let mut total_bytes = 0u64;
    while let Some((directory, archive_path, depth)) = queue.pop_front() {
        ensure_not_cancelled(cancellation)?;
        let current_attributes = session
            .raw
            .lstat(directory.clone())
            .await
            .map_err(|error| map_sftp_error("确认远程下载目录", error))?
            .attrs;
        if !current_attributes.is_dir() {
            return Err(DomainError::Forbidden("目录内容已变化，请重新下载".into()));
        }
        charge_path_bytes(
            &mut retained_bytes,
            &directory,
            &archive_path,
            MAX_REMOTE_ARCHIVE_RETAINED_BYTES,
        )?;
        entries.push(ArchiveEntry {
            remote_path: directory.clone(),
            archive_path: archive_path.clone(),
            attributes: archive_attributes(&current_attributes),
            kind: ArchiveKind::Directory,
        });
        if entries.len() > MAX_REMOTE_ARCHIVE_ENTRIES {
            return Err(archive_entry_limit());
        }
        let remaining = MAX_REMOTE_ARCHIVE_ENTRIES.saturating_sub(entries.len());
        let mut children = read_directory_files(session, &directory, remaining).await?;
        children.sort_by(|left, right| left.filename.cmp(&right.filename));
        for child in children {
            ensure_not_cancelled(cancellation)?;
            validate_remote_name(&child.filename)
                .map_err(|error| DomainError::Other(format!("远端返回了无效文件名：{error}")))?;
            let child_remote =
                join_remote_path(&directory, &child.filename).map_err(DomainError::Other)?;
            if !seen.insert(child_remote.clone()) {
                return Err(DomainError::Forbidden(
                    "远端目录返回了重复路径，已停止下载".into(),
                ));
            }
            let child_archive = archive_path.join(&child.filename);
            charge_path_bytes(
                &mut retained_bytes,
                &child_remote,
                &child_archive,
                MAX_REMOTE_ARCHIVE_RETAINED_BYTES,
            )?;
            if child.attrs.is_dir() {
                if depth >= MAX_REMOTE_ARCHIVE_DEPTH {
                    return Err(DomainError::Forbidden(format!(
                        "目录深度超过 {MAX_REMOTE_ARCHIVE_DEPTH} 层"
                    )));
                }
                queue.push_back((child_remote, child_archive, depth + 1));
                continue;
            }
            let kind = if child.attrs.is_regular() {
                total_bytes = total_bytes.checked_add(child.attrs.len()).ok_or_else(|| {
                    DomainError::Forbidden("目录文件总大小溢出，已停止下载".into())
                })?;
                ArchiveKind::File
            } else if child.attrs.is_symlink() {
                let target = read_link(session, &child_remote).await?;
                validate_link_target(&target)?;
                charge_retained_bytes(
                    &mut retained_bytes,
                    target.len(),
                    MAX_REMOTE_ARCHIVE_RETAINED_BYTES,
                )?;
                ArchiveKind::Symlink(target)
            } else {
                return Err(DomainError::Forbidden(format!(
                    "目录含不支持的特殊文件：{}",
                    child.filename
                )));
            };
            entries.push(ArchiveEntry {
                remote_path: child_remote,
                archive_path: child_archive,
                attributes: archive_attributes(&child.attrs),
                kind,
            });
            if entries.len() > MAX_REMOTE_ARCHIVE_ENTRIES {
                return Err(archive_entry_limit());
            }
        }
    }
    Ok(ArchivePlan {
        entries,
        total_bytes,
    })
}

async fn read_link(session: &StructuredSftpSession, path: &str) -> Result<String> {
    let response = session
        .raw
        .readlink(path.to_string())
        .await
        .map_err(|error| map_sftp_error("读取远程符号链接", error))?;
    response
        .files
        .first()
        .map(|file| file.filename.clone())
        .ok_or_else(|| DomainError::Other("远端未返回符号链接目标".into()))
}

fn validate_link_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.len() > MAX_SSH_PATH_BYTES
        || target.starts_with('/')
        || target.contains('\0')
        || target.contains('\\')
        || target.split('/').any(|part| part == "..")
        || target
            .split('/')
            .next()
            .is_some_and(|part| part.ends_with(':'))
    {
        return Err(DomainError::Forbidden(
            "目录含可能越界的符号链接，已停止下载".into(),
        ));
    }
    Ok(())
}

async fn write_archive(
    session: Arc<StructuredSftpSession>,
    temporary: &Path,
    plan: ArchivePlan,
    cancellation: TransferCancellation,
    progress: SshProgressFn,
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
    let opened =
        match await_cancellable_sftp(session.raw.fstat(handle.clone()), &cancellation).await {
            Ok(Ok(metadata)) => metadata.attrs,
            Ok(Err(error)) => {
                let _ = session.raw.close(handle).await;
                return Err(map_sftp_error("确认远程归档文件", error));
            }
            Err(error) => {
                let _ = session.raw.close(handle).await;
                return Err(error);
            }
        };
    if !opened.is_regular() || opened.len() != entry.attributes.len() {
        let _ = session.raw.close(handle).await;
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
        },
        |mut state| async move {
            if state.offset == state.size {
                return Ok(None);
            }
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

fn archive_entry_limit() -> DomainError {
    DomainError::Forbidden(format!("目录项目超过 {MAX_REMOTE_ARCHIVE_ENTRIES} 个"))
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
mod tests {
    use super::*;

    #[test]
    fn archive_links_cannot_escape_target_directory() {
        for target in ["/etc/passwd", "../secret", "a/../../secret", "a\\secret"] {
            assert!(validate_link_target(target).is_err(), "{target}");
        }
        assert!(validate_link_target("config/current.yml").is_ok());
    }

    #[test]
    fn archive_path_budget_is_bounded() {
        let mut retained = 0;
        let path = async_std::path::Path::new("root/file");
        assert!(charge_path_bytes(&mut retained, "/root/file", path, 1024).is_ok());
        let limit = retained;
        assert!(charge_path_bytes(&mut retained, "/root/file", path, limit).is_err());
    }
}
