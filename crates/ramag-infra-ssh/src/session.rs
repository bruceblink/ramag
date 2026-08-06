//! 结构化 SFTP 远程文件操作。

mod connection;
mod file;

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, stream};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, StatusCode};

use ramag_domain::entities::{
    MAX_REMOTE_DELETE_DEPTH, MAX_REMOTE_DELETE_ENTRIES, MAX_REMOTE_DELETE_RETAINED_BYTES,
    MAX_REMOTE_DIRECTORY_ENTRIES, MAX_REMOTE_DIRECTORY_RETAINED_BYTES, RemoteDirectory,
    RemoteEntry, RemoteEntryKind, RemotePath, contains_case_insensitive, infer_sftp_namespace,
    join_remote_path, validate_remote_name, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};

pub use connection::{SessionCache, SftpConnection, SftpTransport, StructuredSftpSession};
pub use file::{read_file_chunk, read_file_preview};

const WINDOWS_DRIVE_PROBE_CONCURRENCY: usize = 4;

pub async fn list_directory(
    session: &StructuredSftpSession,
    path: &str,
    max_entries: usize,
) -> Result<RemoteDirectory> {
    validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
    let canonical = session
        .canonicalize(path.to_string())
        .await
        .map_err(|error| map_sftp_error("解析远程目录", error))?;
    validate_remote_path(&canonical)
        .map_err(|error| DomainError::Other(format!("远端返回了无效的规范路径：{error}")))?;
    let max_entries = max_entries.min(MAX_REMOTE_DIRECTORY_ENTRIES);
    let (canonical, directory) = if should_preserve_windows_directory_request(path, &canonical) {
        match read_windows_directory_request(session, path, max_entries).await {
            Ok(directory) => (path.to_string(), directory),
            Err(_) => {
                let directory = read_directory_files(session, &canonical, max_entries).await?;
                (canonical, directory)
            }
        }
    } else {
        let directory = read_directory_files(session, &canonical, max_entries).await?;
        (canonical, directory)
    };
    let mut entries = Vec::new();
    let mut retained_bytes = 0usize;
    for entry in directory {
        let name = entry.filename;
        validate_remote_name(&name)
            .map_err(|error| DomainError::Other(format!("远端返回了无效文件名：{error}")))?;
        let metadata = entry.attrs;
        let kind = entry_kind(&metadata);
        let modified_at = metadata
            .mtime
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(i64::from(seconds), 0));
        let path = join_remote_path(&canonical, &name).map_err(DomainError::Other)?;
        charge_retained_bytes(
            &mut retained_bytes,
            std::mem::size_of::<RemoteEntry>()
                .saturating_add(name.len())
                .saturating_add(path.len()),
            MAX_REMOTE_DIRECTORY_RETAINED_BYTES,
            "远程目录渲染数据",
        )?;
        entries.push(RemoteEntry {
            path,
            name,
            kind,
            size: metadata.len(),
            permissions: metadata.permissions,
            modified_at,
        });
    }
    entries.sort_by(|left, right| {
        entry_rank(left.kind)
            .cmp(&entry_rank(right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(RemoteDirectory {
        path: canonical,
        entries,
    })
}

/// SFTP 没有统一的 Windows 盘符枚举命令；对空虚拟根目录做有界并发探测。
pub async fn list_windows_drives(session: &StructuredSftpSession) -> Vec<RemoteEntry> {
    let mut drives = stream::iter(b'A'..=b'Z')
        .map(|letter| async move { accessible_windows_drive(session, letter).await })
        .buffer_unordered(WINDOWS_DRIVE_PROBE_CONCURRENCY)
        .filter_map(|drive| async move { drive })
        .collect::<Vec<_>>()
        .await;
    drives.sort_unstable();
    drives
        .into_iter()
        .map(|path| RemoteEntry {
            name: path
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string(),
            path,
            kind: RemoteEntryKind::Directory,
            size: 0,
            permissions: None,
            modified_at: None,
        })
        .collect()
}

async fn accessible_windows_drive(session: &StructuredSftpSession, letter: u8) -> Option<String> {
    let native = format!("{}:/", char::from(letter));
    let virtual_path = format!("/{native}");
    for path in [&virtual_path, &native] {
        if session
            .raw
            .lstat(path.clone())
            .await
            .is_ok_and(|metadata| metadata.attrs.is_dir())
        {
            return Some(virtual_path.clone());
        }
        let Ok(handle) = session.raw.opendir(path.clone()).await else {
            continue;
        };
        if session.raw.close(handle.handle).await.is_ok() {
            return Some(virtual_path.clone());
        }
    }
    None
}

async fn read_windows_directory_request(
    session: &StructuredSftpSession,
    requested: &str,
    max_entries: usize,
) -> Result<Vec<russh_sftp::protocol::File>> {
    if let Ok(directory) = read_directory_files(session, requested, max_entries).await {
        return Ok(directory);
    }
    let alternate = requested
        .strip_prefix('/')
        .map(str::to_string)
        .unwrap_or_else(|| format!("/{requested}"));
    read_directory_files(session, &alternate, max_entries).await
}

/// 部分 Windows SFTP 代理会把有效盘符路径的 realpath 错误折叠为 `/`。
/// 仅在显式盘符路径可直接打开时保留请求形式，避免把用户目录误显示为空根目录。
fn should_preserve_windows_directory_request(requested: &str, canonical: &str) -> bool {
    if canonical != "/" {
        return false;
    }
    let path = requested.strip_prefix('/').unwrap_or(requested);
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

pub async fn create_directory(session: &StructuredSftpSession, path: &str) -> Result<()> {
    session
        .raw
        .mkdir(path.to_string(), FileAttributes::empty())
        .await
        .map(|_| ())
        .map_err(|error| map_sftp_error("创建远程目录", error))
}

pub async fn rename(session: &StructuredSftpSession, old_path: &str, new_path: &str) -> Result<()> {
    session
        .raw
        .rename(old_path.to_string(), new_path.to_string())
        .await
        .map(|_| ())
        .map_err(|error| map_sftp_error("重命名远程项目", error))
}

pub async fn remove(
    session: &StructuredSftpSession,
    path: &str,
    kind: RemoteEntryKind,
) -> Result<()> {
    validate_safe_delete_path(path)?;
    let current_kind = session
        .raw
        .lstat(path.to_string())
        .await
        .map(|metadata| entry_kind(&metadata.attrs))
        .map_err(|error| map_sftp_error("确认待删除远程项目类型", error))?;
    if current_kind != kind {
        return Err(DomainError::Forbidden(
            "远程项目类型已变化；为避免误删，请刷新目录后重新确认".into(),
        ));
    }
    if current_kind != RemoteEntryKind::Directory {
        return session
            .raw
            .remove(path.to_string())
            .await
            .map(|_| ())
            .map_err(|error| map_sftp_error("删除远程文件", error));
    }

    let mut seen = HashSet::from([path.to_string()]);
    let mut retained_path_bytes = 0usize;
    charge_retained_bytes(
        &mut retained_path_bytes,
        delete_path_cost(path),
        MAX_REMOTE_DELETE_RETAINED_BYTES,
        "递归删除路径",
    )?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut stack = vec![(path.to_string(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_REMOTE_DELETE_DEPTH {
            return Err(DomainError::Forbidden(format!(
                "递归删除深度超过 {MAX_REMOTE_DELETE_DEPTH} 层安全上限"
            )));
        }
        let current_metadata = session
            .raw
            .lstat(current.clone())
            .await
            .map_err(|error| map_sftp_error("确认递归删除项目类型", error))?;
        if !current_metadata.attrs.is_dir() {
            files.push(current);
            continue;
        }
        directories.push((current.clone(), depth));
        let remaining = MAX_REMOTE_DELETE_ENTRIES.saturating_sub(seen.len());
        let entries = read_directory_files(session, &current, remaining).await?;
        for entry in entries {
            let name = entry.filename;
            validate_remote_name(&name)
                .map_err(|error| DomainError::Other(format!("远端返回了无效文件名：{error}")))?;
            let child = join_remote_path(&current, &name).map_err(DomainError::Other)?;
            if !seen.insert(child.clone()) {
                return Err(DomainError::Forbidden(
                    "远端目录返回了重复路径，已停止递归删除".into(),
                ));
            }
            charge_retained_bytes(
                &mut retained_path_bytes,
                delete_path_cost(&child),
                MAX_REMOTE_DELETE_RETAINED_BYTES,
                "递归删除路径",
            )?;
            if seen.len() > MAX_REMOTE_DELETE_ENTRIES {
                return Err(DomainError::Forbidden(format!(
                    "递归删除项目超过 {MAX_REMOTE_DELETE_ENTRIES} 个安全上限"
                )));
            }
            if entry.attrs.is_dir() {
                stack.push((child, depth + 1));
            } else {
                files.push(child);
            }
        }
    }

    // 完整预扫描通过后再执行删除，避免命中资源上限时只删掉一半。
    for file in files {
        let metadata = session
            .raw
            .lstat(file.clone())
            .await
            .map_err(|error| map_sftp_error("再次确认待删除远程文件类型", error))?;
        if metadata.attrs.is_dir() {
            return Err(DomainError::Forbidden(
                "远程项目类型在确认后发生变化；已停止递归删除".into(),
            ));
        }
        session
            .raw
            .remove(file)
            .await
            .map_err(|error| map_sftp_error("删除远程文件", error))?;
    }
    directories.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));
    for (directory, _) in directories {
        let metadata = session
            .raw
            .lstat(directory.clone())
            .await
            .map_err(|error| map_sftp_error("再次确认待删除远程目录类型", error))?;
        if !metadata.attrs.is_dir() {
            return Err(DomainError::Forbidden(
                "远程项目类型在确认后发生变化；已停止递归删除".into(),
            ));
        }
        session
            .raw
            .rmdir(directory)
            .await
            .map_err(|error| map_sftp_error("删除远程目录", error))?;
    }
    Ok(())
}

pub(crate) async fn read_directory_files(
    session: &StructuredSftpSession,
    path: &str,
    max_entries: usize,
) -> Result<Vec<russh_sftp::protocol::File>> {
    let handle = session
        .raw
        .opendir(path.to_string())
        .await
        .map_err(|error| map_sftp_error("打开远程目录", error))?
        .handle;
    let mut entries = Vec::new();
    let mut retained_bytes = 0usize;
    let mut packets = 0usize;
    let result = async {
        loop {
            packets += 1;
            if packets > max_entries.saturating_add(32) {
                return Err(DomainError::Other(
                    "远端目录返回了过多空数据包，已停止读取".into(),
                ));
            }
            match session.raw.readdir(handle.clone()).await {
                Ok(batch) => {
                    if batch.files.is_empty() {
                        return Err(DomainError::Other(
                            "远端目录返回了空数据包而非结束标记".into(),
                        ));
                    }
                    for entry in batch.files {
                        if matches!(entry.filename.as_str(), "." | "..") {
                            continue;
                        }
                        if entries.len() >= max_entries {
                            return Err(DomainError::Forbidden(format!(
                                "远程目录条目超过 {max_entries} 个安全上限"
                            )));
                        }
                        charge_retained_bytes(
                            &mut retained_bytes,
                            raw_entry_retained_bytes(&entry),
                            MAX_REMOTE_DIRECTORY_RETAINED_BYTES,
                            "远程目录协议数据",
                        )?;
                        entries.push(entry);
                    }
                }
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => break,
                Err(error) => return Err(map_sftp_error("读取远程目录", error)),
            }
        }
        Ok(entries)
    }
    .await;
    if let Err(error) = session.raw.close(handle).await {
        let close_error = map_sftp_error("关闭远程目录", error);
        if result.is_ok() || matches!(close_error, DomainError::ConnectionFailed(_)) {
            return Err(close_error);
        }
        tracing::warn!(error = %close_error, "close ssh sftp directory handle failed");
    }
    result
}

fn raw_entry_retained_bytes(entry: &russh_sftp::protocol::File) -> usize {
    std::mem::size_of::<russh_sftp::protocol::File>()
        .saturating_add(entry.filename.len())
        .saturating_add(entry.longname.len())
        .saturating_add(entry.attrs.user.as_ref().map_or(0, String::len))
        .saturating_add(entry.attrs.group.as_ref().map_or(0, String::len))
}

fn delete_path_cost(path: &str) -> usize {
    // `seen` 与待删除列表各保留一份路径，并为 HashSet / Vec 节点预留固定开销。
    (std::mem::size_of::<String>() + path.len())
        .saturating_mul(2)
        .saturating_add(64)
}

fn charge_retained_bytes(
    retained: &mut usize,
    additional: usize,
    limit: usize,
    label: &str,
) -> Result<()> {
    let next = retained
        .checked_add(additional)
        .ok_or_else(|| DomainError::Forbidden(format!("{label}大小溢出，已停止处理远端数据")))?;
    if next > limit {
        return Err(DomainError::Forbidden(format!(
            "{label}超过 {} MiB 安全上限",
            limit / 1024 / 1024
        )));
    }
    *retained = next;
    Ok(())
}

fn validate_safe_delete_path(path: &str) -> Result<()> {
    let path = RemotePath::parse_with_namespace(path, infer_sftp_namespace(path))
        .map_err(DomainError::InvalidConfig)?;
    if path.is_root() {
        return Err(DomainError::Forbidden("禁止删除远程根目录".into()));
    }
    Ok(())
}

pub fn map_sftp_error(context: &str, error: SftpError) -> DomainError {
    match &error {
        SftpError::Timeout
        | SftpError::IO(_)
        | SftpError::UnexpectedPacket
        | SftpError::UnexpectedBehavior(_) => {
            DomainError::ConnectionFailed(format!("{context}失败：{error}"))
        }
        SftpError::Status(status)
            if matches!(
                status.status_code,
                StatusCode::NoConnection | StatusCode::ConnectionLost
            ) =>
        {
            DomainError::ConnectionFailed(format!("{context}失败：{error}"))
        }
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => {
            DomainError::NotFound(format!("{context}的远程项目"))
        }
        SftpError::Status(status)
            if status.status_code == StatusCode::PermissionDenied
                || contains_case_insensitive(&status.error_message, "permission denied") =>
        {
            DomainError::Forbidden(format!("{context}：权限不足"))
        }
        _ => DomainError::Other(format!("{context}失败：{error}")),
    }
}

fn entry_kind(metadata: &FileAttributes) -> RemoteEntryKind {
    if metadata.is_dir() {
        RemoteEntryKind::Directory
    } else if metadata.is_symlink() {
        RemoteEntryKind::Symlink
    } else if metadata.is_regular() {
        RemoteEntryKind::File
    } else {
        RemoteEntryKind::Other
    }
}

fn entry_rank(kind: RemoteEntryKind) -> u8 {
    match kind {
        RemoteEntryKind::Directory => 0,
        RemoteEntryKind::File => 1,
        RemoteEntryKind::Symlink => 2,
        RemoteEntryKind::Other => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::FileMode;

    #[test]
    fn protocol_disconnects_are_classified_as_connection_failures() {
        for error in [
            SftpError::Timeout,
            SftpError::UnexpectedPacket,
            SftpError::UnexpectedBehavior("session closed".into()),
        ] {
            assert!(matches!(
                map_sftp_error("读取", error),
                DomainError::ConnectionFailed(_)
            ));
        }
    }

    #[test]
    fn generic_sftp_permission_error_has_a_concise_hint() {
        let error = SftpError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::Failure,
            error_message: "permission denied".into(),
            language_tag: String::new(),
        });

        assert!(matches!(
            map_sftp_error("打开远程目录", error),
            DomainError::Forbidden(message) if message == "打开远程目录：权限不足"
        ));
    }

    #[test]
    fn entry_kind_does_not_treat_symlink_as_directory() {
        let mut metadata = FileAttributes::empty();
        metadata.set_type(FileMode::LNK);
        assert_eq!(entry_kind(&metadata), RemoteEntryKind::Symlink);
    }

    #[test]
    fn windows_drive_request_survives_a_broken_root_realpath() {
        assert!(should_preserve_windows_directory_request(
            "C:/Users/Administrator",
            "/"
        ));
        assert!(should_preserve_windows_directory_request(
            "/C:/Users/Administrator",
            "/"
        ));
        assert!(!should_preserve_windows_directory_request(
            "/home/alice",
            "/"
        ));
        assert!(!should_preserve_windows_directory_request(
            "C:/Users", "C:/Users"
        ));
    }

    #[test]
    fn destructive_paths_reject_root_relative_and_parent_aliases() {
        for path in ["/", "//", ".", "relative", "/.", "/tmp/../root"] {
            assert!(validate_safe_delete_path(path).is_err(), "{path}");
        }
        assert!(validate_safe_delete_path("/home/alice/file").is_ok());
    }

    #[test]
    fn retained_byte_budget_rejects_oversized_remote_data() {
        let mut retained = 8usize;
        assert!(charge_retained_bytes(&mut retained, 8, 16, "test").is_ok());
        assert_eq!(retained, 16);
        assert!(charge_retained_bytes(&mut retained, 1, 16, "test").is_err());
        assert!(delete_path_cost("/remote/file") > "/remote/file".len());
    }
}
