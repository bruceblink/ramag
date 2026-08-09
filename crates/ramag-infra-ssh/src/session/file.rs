//! 远程普通文件的有界分段读取。

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};

use ramag_domain::entities::{
    MAX_REMOTE_FILE_PREVIEW_BYTES, RemoteFileChunk, RemoteFileChunkPosition, RemoteFilePreview,
    validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};

use super::{StructuredSftpSession, map_sftp_error};

pub async fn read_file_preview(
    session: &StructuredSftpSession,
    path: &str,
) -> Result<RemoteFilePreview> {
    let chunk = read_file_chunk(session, path, RemoteFileChunkPosition::From(0)).await?;
    let truncated = chunk.end_offset() < chunk.total_bytes;
    Ok(RemoteFilePreview {
        bytes: chunk.bytes,
        total_bytes: chunk.total_bytes,
        truncated,
    })
}

pub async fn read_file_chunk(
    session: &StructuredSftpSession,
    path: &str,
    position: RemoteFileChunkPosition,
) -> Result<RemoteFileChunk> {
    validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
    let metadata = session
        .raw
        .lstat(path.to_string())
        .await
        .map_err(|error| map_sftp_error("读取远程文件信息", error))?;
    if !metadata.attrs.is_regular() {
        return Err(DomainError::InvalidConfig(
            "仅支持查看普通文件，不跟随符号链接".into(),
        ));
    }
    let handle = session
        .raw
        .open(path.to_string(), OpenFlags::READ, FileAttributes::empty())
        .await
        .map_err(|error| map_sftp_error("打开远程文件", error))?
        .handle;
    let result = read_open_file_chunk(session, &handle, position).await;
    if let Err(error) = session.raw.close(handle).await {
        let close_error = map_sftp_error("关闭远程文件", error);
        if result.is_ok() || matches!(close_error, DomainError::ConnectionFailed(_)) {
            return Err(close_error);
        }
        tracing::warn!(
            operation = "ssh_sftp_file_close",
            error = %close_error,
            "close ssh sftp file handle failed"
        );
    }
    result
}

async fn read_open_file_chunk(
    session: &StructuredSftpSession,
    handle: &str,
    position: RemoteFileChunkPosition,
) -> Result<RemoteFileChunk> {
    let metadata = session
        .raw
        .fstat(handle.to_string())
        .await
        .map_err(|error| map_sftp_error("确认远程文件", error))?;
    if !metadata.attrs.is_regular() {
        return Err(DomainError::Forbidden(
            "远程文件在打开时已不再是普通文件".into(),
        ));
    }
    let total = metadata
        .attrs
        .size
        .ok_or_else(|| DomainError::Other("远端未返回文件大小，无法执行有界读取".into()))?;
    let (start, end) = chunk_range(position, total);
    let expected = usize::try_from(end - start)
        .map_err(|_| DomainError::Other("远程文件片段大小溢出".into()))?;
    let mut bytes = Vec::with_capacity(expected);
    while bytes.len() < expected {
        let remaining = expected - bytes.len();
        let request_bytes = remaining.min(session.read_chunk_bytes.max(1) as usize) as u32;
        let offset = start
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| DomainError::Other("远程文件读取偏移溢出".into()))?;
        let data = match session
            .raw
            .read(handle.to_string(), offset, request_bytes)
            .await
        {
            Ok(data) => data.data,
            Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                return Err(DomainError::ConnectionFailed(
                    "远程文件读取期间发生变化，请重试".into(),
                ));
            }
            Err(error) => return Err(map_sftp_error("读取远程文件", error)),
        };
        if data.is_empty() {
            return Err(DomainError::ConnectionFailed(
                "远端返回了空文件数据包而非结束标记".into(),
            ));
        }
        if data.len() > remaining {
            return Err(DomainError::ConnectionFailed(
                "远端返回的文件数据超过请求上限".into(),
            ));
        }
        bytes.extend_from_slice(&data);
    }
    Ok(RemoteFileChunk {
        bytes,
        offset: start,
        total_bytes: total,
    })
}

fn chunk_range(position: RemoteFileChunkPosition, total: u64) -> (u64, u64) {
    let limit = MAX_REMOTE_FILE_PREVIEW_BYTES as u64;
    match position {
        RemoteFileChunkPosition::From(offset) => {
            let start = offset.min(total);
            (start, start.saturating_add(limit).min(total))
        }
        RemoteFileChunkPosition::Before(offset) => {
            let end = offset.min(total);
            (end.saturating_sub(limit), end)
        }
        RemoteFileChunkPosition::Tail => (total.saturating_sub(limit), total),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_ranges_are_bounded_and_clamped() {
        let limit = MAX_REMOTE_FILE_PREVIEW_BYTES as u64;
        let total = limit * 3 + 17;

        assert_eq!(
            chunk_range(RemoteFileChunkPosition::From(limit), total),
            (limit, limit * 2)
        );
        assert_eq!(
            chunk_range(RemoteFileChunkPosition::Before(limit * 2), total),
            (limit, limit * 2)
        );
        assert_eq!(
            chunk_range(RemoteFileChunkPosition::Tail, total),
            (total - limit, total)
        );
        assert_eq!(
            chunk_range(RemoteFileChunkPosition::From(total + 1), total),
            (total, total)
        );
        assert_eq!(
            chunk_range(RemoteFileChunkPosition::Before(limit / 2), total),
            (0, limit / 2)
        );
    }

    #[test]
    fn small_file_is_returned_as_one_chunk() {
        assert_eq!(chunk_range(RemoteFileChunkPosition::Tail, 42), (0, 42));
    }
}
