//! 远程目录归档扫描。

use super::*;

pub(super) async fn scan_directory(
    session: &StructuredSftpSession,
    remote_path: &str,
    cancellation: &TransferCancellation,
    production: bool,
    deadline: Option<std::time::Instant>,
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
    let canonical_path =
        RemotePath::parse_with_namespace(&canonical, infer_sftp_namespace(&canonical))
            .map_err(DomainError::InvalidConfig)?;
    let root_name = if canonical_path.is_root() {
        match canonical_path.namespace() {
            SftpNamespaceKind::WindowsDrive => "drive-root",
            SftpNamespaceKind::Posix | SftpNamespaceKind::Virtual | SftpNamespaceKind::Unknown => {
                "root"
            }
        }
    } else {
        canonical
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("root")
    };
    validate_remote_name(root_name).map_err(DomainError::InvalidConfig)?;

    let root_archive = async_std::path::PathBuf::from(root_name);
    let max_entries = if production {
        MAX_PRODUCTION_DIRECTORY_ENTRIES
    } else {
        MAX_REMOTE_ARCHIVE_ENTRIES
    };
    let max_bytes = if production {
        MAX_PRODUCTION_DOWNLOAD_BYTES
    } else {
        u64::MAX
    };
    let mut queue = VecDeque::from([(canonical.clone(), root_archive.clone(), 0usize)]);
    let mut seen = HashSet::from([canonical]);
    let mut entries = Vec::new();
    let mut retained_bytes = 0usize;
    let mut total_bytes = 0u64;
    while let Some((directory, archive_path, depth)) = queue.pop_front() {
        ensure_archive_deadline(deadline)?;
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
        if entries.len() > max_entries {
            return Err(archive_entry_limit(max_entries));
        }
        let remaining = max_entries.saturating_sub(entries.len());
        let mut children = read_directory_files(session, &directory, remaining).await?;
        children.sort_by(|left, right| left.filename.cmp(&right.filename));
        for child in children {
            ensure_archive_deadline(deadline)?;
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
                if total_bytes > max_bytes {
                    return Err(DomainError::Forbidden(format!(
                        "远程目录文件总大小超过 {} MiB 生产下载上限",
                        max_bytes / 1024 / 1024
                    )));
                }
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
            if entries.len() > max_entries {
                return Err(archive_entry_limit(max_entries));
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

pub(super) fn validate_link_target(target: &str) -> Result<()> {
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
