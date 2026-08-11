//! 对象上传与下载；写入失败或取消时显式清理未完成数据。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use opendal::Operator;
use ramag_domain::entities::{
    OBJECT_STORAGE_TRANSFER_BUFFER_BYTES, ObjectDownloadRequest, ObjectTransferProgress,
    ObjectUploadRequest, OverwritePolicy, is_opendal_safe_key,
};
use ramag_domain::error::ObjectStorageResult;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::errors::{cancelled, conflict, invalid, map_io, map_opendal};

pub async fn upload(
    operator: Arc<Operator>,
    request: ObjectUploadRequest,
) -> ObjectStorageResult<()> {
    ensure_write_allowed(request.account.read_only, &request.key, "upload")?;
    ensure_absolute(&request.local_path, "upload")?;
    let metadata = tokio::fs::symlink_metadata(&request.local_path)
        .await
        .map_err(|error| map_io("upload", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid("upload", "上传源必须是普通文件，不能是符号链接"));
    }
    let total = metadata.len();
    let mut file = tokio::fs::File::open(&request.local_path)
        .await
        .map_err(|error| map_io("upload", error))?;
    let mut writer = operator
        .writer_with(&request.key)
        .if_not_exists(request.overwrite == OverwritePolicy::Refuse)
        .await
        .map_err(|error| map_opendal("upload", error))?;
    let mut transferred = 0u64;
    let mut buffer = vec![0; OBJECT_STORAGE_TRANSFER_BUFFER_BYTES];
    loop {
        if request.cancellation.is_cancelled() {
            abort_writer(&mut writer).await;
            return Err(cancelled("upload"));
        }
        let read = match file.read(&mut buffer).await {
            Ok(read) => read,
            Err(error) => {
                abort_writer(&mut writer).await;
                return Err(map_io("upload", error));
            }
        };
        if read == 0 {
            break;
        }
        if let Err(error) = writer.write(Bytes::copy_from_slice(&buffer[..read])).await {
            abort_writer(&mut writer).await;
            return Err(map_opendal("upload", error));
        }
        transferred = transferred.saturating_add(read as u64);
        (request.progress)(ObjectTransferProgress { transferred, total });
    }
    if request.cancellation.is_cancelled() {
        abort_writer(&mut writer).await;
        return Err(cancelled("upload"));
    }
    writer
        .close()
        .await
        .map_err(|error| map_opendal("upload", error))?;
    Ok(())
}

pub async fn download(
    operator: Arc<Operator>,
    request: ObjectDownloadRequest,
) -> ObjectStorageResult<()> {
    ensure_safe_key(&request.key, "download")?;
    ensure_absolute(&request.local_path, "download")?;
    validate_download_target(&request.local_path, request.overwrite).await?;
    let metadata = operator
        .stat(&request.key)
        .await
        .map_err(|error| map_opendal("download", error))?;
    let total = metadata.content_length();
    let temporary = temporary_path(&request.local_path)?;
    let result = download_to_temp(operator, &request, &temporary, total).await;
    if result.is_err() {
        cleanup_temporary_download(&temporary).await;
        return result;
    }
    let result = commit_download(&temporary, &request.local_path, request.overwrite).await;
    if result.is_err() {
        cleanup_temporary_download(&temporary).await;
    }
    result
}

async fn cleanup_temporary_download(temporary: &Path) {
    if let Err(error) = tokio::fs::remove_file(temporary).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            operation = "download_cleanup",
            error_kind = ?error.kind(),
            "Failed to remove temporary download"
        );
    }
}

async fn download_to_temp(
    operator: Arc<Operator>,
    request: &ObjectDownloadRequest,
    temporary: &Path,
    total: u64,
) -> ObjectStorageResult<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .await
        .map_err(|error| map_io("download", error))?;
    let reader = operator
        .reader(&request.key)
        .await
        .map_err(|error| map_opendal("download", error))?;
    let mut stream = reader
        .into_bytes_stream(..)
        .await
        .map_err(|error| map_opendal("download", error))?;
    let mut transferred = 0u64;
    while let Some(chunk) = stream.next().await {
        if request.cancellation.is_cancelled() {
            return Err(cancelled("download"));
        }
        let chunk = chunk.map_err(|error| map_io("download", error))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| map_io("download", error))?;
        transferred = transferred.saturating_add(chunk.len() as u64);
        (request.progress)(ObjectTransferProgress { transferred, total });
    }
    file.flush()
        .await
        .map_err(|error| map_io("download", error))?;
    file.sync_all()
        .await
        .map_err(|error| map_io("download", error))
}

async fn commit_download(
    temporary: &Path,
    target: &Path,
    overwrite: OverwritePolicy,
) -> ObjectStorageResult<()> {
    if overwrite == OverwritePolicy::Refuse {
        tokio::fs::hard_link(temporary, target)
            .await
            .map_err(|error| map_io("download", error))?;
        tokio::fs::remove_file(temporary)
            .await
            .map_err(|error| map_io("download", error))?;
        return Ok(());
    }
    replace_download_target(temporary, target).await
}

#[cfg(not(target_os = "windows"))]
async fn replace_download_target(temporary: &Path, target: &Path) -> ObjectStorageResult<()> {
    tokio::fs::rename(temporary, target)
        .await
        .map_err(|error| map_io("download", error))
}

#[cfg(target_os = "windows")]
async fn replace_download_target(temporary: &Path, target: &Path) -> ObjectStorageResult<()> {
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
    .map_err(|error| map_io("download", std::io::Error::other(error)))
}

async fn validate_download_target(
    target: &Path,
    overwrite: OverwritePolicy,
) -> ObjectStorageResult<()> {
    match tokio::fs::symlink_metadata(target).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(invalid("download", "下载目标不能是符号链接"))
        }
        Ok(metadata) if !metadata.is_file() => Err(invalid("download", "下载目标不是普通文件")),
        Ok(_) if overwrite == OverwritePolicy::Refuse => {
            Err(conflict("download", "下载目标已存在"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(map_io("download", error)),
    }
}

fn temporary_path(target: &Path) -> ObjectStorageResult<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| invalid("download", "下载目标缺少父目录"))?;
    Ok(parent.join(format!(
        ".ramag-download-{}.part",
        uuid::Uuid::new_v4().simple()
    )))
}

fn ensure_write_allowed(
    read_only: bool,
    key: &str,
    operation: &'static str,
) -> ObjectStorageResult<()> {
    if read_only {
        return Err(invalid(operation, "生产模式下不能执行写操作"));
    }
    ensure_safe_key(key, operation)
}

fn ensure_safe_key(key: &str, operation: &'static str) -> ObjectStorageResult<()> {
    if is_opendal_safe_key(key) {
        Ok(())
    } else {
        Err(invalid(operation, "当前对象键无法由 OpenDAL 安全表示"))
    }
}

fn ensure_absolute(path: &Path, operation: &'static str) -> ObjectStorageResult<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(invalid(operation, "本地文件路径必须是绝对路径"))
    }
}

async fn abort_writer(writer: &mut opendal::Writer) {
    if writer.abort().await.is_err() {
        tracing::warn!(operation = "upload", "Failed to abort object writer");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_overwrite_commit_is_atomic_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("download.part");
        let target = directory.path().join("download.txt");
        tokio::fs::write(&temporary, b"content").await.unwrap();

        commit_download(&temporary, &target, OverwritePolicy::Refuse)
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"content");
        assert!(tokio::fs::symlink_metadata(&temporary).await.is_err());
    }

    #[tokio::test]
    async fn no_overwrite_rejects_existing_target_without_modifying_it() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("download.part");
        let target = directory.path().join("download.txt");
        tokio::fs::write(&temporary, b"new").await.unwrap();
        tokio::fs::write(&target, b"existing").await.unwrap();

        let result = commit_download(&temporary, &target, OverwritePolicy::Refuse).await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"existing");
        assert_eq!(tokio::fs::read(&temporary).await.unwrap(), b"new");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_target_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual.txt");
        let target = directory.path().join("target.txt");
        tokio::fs::write(&actual, b"existing").await.unwrap();
        symlink(&actual, &target).unwrap();

        assert!(
            validate_download_target(&target, OverwritePolicy::Overwrite)
                .await
                .is_err()
        );
    }
}
