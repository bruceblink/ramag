//! 临时文件路径与本地、远程原子提交。

use std::path::{Path, PathBuf};

use uuid::Uuid;

use ramag_domain::entities::{OverwritePolicy, parent_remote_path, validate_remote_path};
use ramag_domain::error::{DomainError, Result};

use crate::session::{StructuredSftpSession, map_sftp_error};

pub(super) fn remote_sibling(target: &str, marker: &str) -> Result<String> {
    validate_remote_path(target).map_err(DomainError::InvalidConfig)?;
    let parent = parent_remote_path(target).map_err(DomainError::InvalidConfig)?;
    let file_name = target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != ".")
        .ok_or_else(|| DomainError::InvalidConfig("远程传输目标缺少文件名".into()))?;
    let temporary_name = format!(".{file_name}.{marker}-{}.tmp", Uuid::new_v4());
    if parent == "/" {
        Ok(format!("/{temporary_name}"))
    } else {
        Ok(format!("{}/{temporary_name}", parent.trim_end_matches('/')))
    }
}

pub(super) fn local_sibling(target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| DomainError::InvalidConfig("本地下载目标缺少父目录".into()))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DomainError::InvalidConfig("本地下载目标缺少 UTF-8 文件名".into()))?;
    Ok(parent.join(format!(".{name}.ramag-download-{}.tmp", Uuid::new_v4())))
}

pub(super) async fn commit_remote(
    session: &StructuredSftpSession,
    temporary: &str,
    target: &str,
    target_existed: bool,
) -> Result<()> {
    if !target_existed {
        return session
            .raw
            .rename(temporary.to_string(), target.to_string())
            .await
            .map(|_| ())
            .map_err(|error| map_sftp_error("提交远程上传文件", error));
    }
    let backup = remote_sibling(target, "ramag-backup")?;
    session
        .raw
        .rename(target.to_string(), backup.clone())
        .await
        .map_err(|error| map_sftp_error("暂存被覆盖的远程文件", error))?;
    let backup_metadata = session.raw.lstat(backup.clone()).await;
    if !matches!(backup_metadata, Ok(ref metadata) if metadata.attrs.is_regular()) {
        let rollback = session.raw.rename(backup.clone(), target.to_string()).await;
        return match (backup_metadata, rollback) {
            (_, Err(rollback_error)) => Err(DomainError::Other(format!(
                "远程目标类型发生变化且恢复原目标失败：{rollback_error}"
            ))),
            (Err(error), Ok(_)) => Err(map_sftp_error("再次确认远程覆盖目标", error)),
            (Ok(_), Ok(_)) => Err(DomainError::Forbidden(
                "远程覆盖目标已不再是普通文件；已恢复原目标".into(),
            )),
        };
    }
    if let Err(error) = session
        .raw
        .rename(temporary.to_string(), target.to_string())
        .await
    {
        let rollback = session.raw.rename(backup.clone(), target.to_string()).await;
        cleanup_remote(session, temporary).await;
        return match rollback {
            Ok(_) => Err(map_sftp_error("提交远程上传文件", error)),
            Err(rollback_error) => Err(DomainError::Other(format!(
                "提交远程上传文件失败且恢复原文件也失败：{error}；{rollback_error}"
            ))),
        };
    }
    session
        .raw
        .remove(backup)
        .await
        .map(|_| ())
        .map_err(|error| map_sftp_error("清理远程覆盖备份", error))
}

pub(super) async fn cleanup_remote(session: &StructuredSftpSession, path: &str) {
    if let Err(error) = session.raw.remove(path.to_string()).await
        && !matches!(
            error,
            russh_sftp::client::error::Error::Status(ref status)
                if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile
        )
    {
        tracing::warn!(error = %error, "cleanup ssh remote temporary file failed");
    }
}

pub(super) async fn cleanup_local(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(error = %error, "cleanup ssh local temporary file failed");
    }
}

pub(super) async fn commit_local(
    temporary: &Path,
    target: &Path,
    overwrite: OverwritePolicy,
) -> Result<()> {
    let temporary = temporary.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || commit_local_blocking(&temporary, &target, overwrite))
        .await
        .map_err(|error| DomainError::Other(format!("提交本地下载任务异常退出：{error}")))?
}

pub(super) fn commit_local_blocking(
    temporary: &Path,
    target: &Path,
    overwrite: OverwritePolicy,
) -> Result<()> {
    if overwrite == OverwritePolicy::Refuse {
        std::fs::hard_link(temporary, target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                DomainError::Forbidden("本地目标已存在；未覆盖原文件".into())
            } else {
                DomainError::Other(format!("原子提交本地下载文件失败：{error}"))
            }
        })?;
        std::fs::remove_file(temporary)
            .map_err(|error| DomainError::Other(format!("清理本地下载临时链接失败：{error}")))?;
        return Ok(());
    }
    replace_local_file(temporary, target)
}

#[cfg(not(target_os = "windows"))]
fn replace_local_file(temporary: &Path, target: &Path) -> Result<()> {
    std::fs::rename(temporary, target)
        .map_err(|error| DomainError::Other(format!("原子替换本地下载文件失败：{error}")))
}

#[cfg(target_os = "windows")]
fn replace_local_file(temporary: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let source = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: 两个 UTF-16 缓冲区均以 NUL 结尾，并在调用期间保持有效。
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| DomainError::Other(format!("原子替换本地下载文件失败：{error}")))
}
