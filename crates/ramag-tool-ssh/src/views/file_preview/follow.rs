//! 日志尾部的低开销增量刷新。

use ramag_app::SshService;
use ramag_domain::entities::{RemoteFileChunkPosition, SshProfile};

use super::super::file_chunk::{RemoteFileText, decode_remote_file_chunk};

pub(super) enum TailUpdate {
    Unchanged,
    Append(RemoteFileText),
    Replace(RemoteFileText),
}

pub(super) fn supports_auto_refresh(path: &str, windowed: bool) -> bool {
    windowed || is_log_path(path)
}

pub(super) fn enables_auto_refresh(path: &str) -> bool {
    is_log_path(path)
}

pub(super) async fn read_tail_update(
    service: &SshService,
    profile: &SshProfile,
    path: &str,
    known_end: u64,
    known_total: u64,
) -> Result<TailUpdate, String> {
    if known_end != known_total {
        return read_tail(service, profile, path).await;
    }
    let position = RemoteFileChunkPosition::From(known_end);
    let chunk = service
        .read_file_chunk(profile, path, position)
        .await
        .map_err(|error| error.to_string())?;
    let preview = decode_remote_file_chunk(chunk, position)?;

    if preview.total_bytes == known_end && preview.offset == known_end {
        return Ok(TailUpdate::Unchanged);
    }
    if preview.offset == known_end && preview.end_offset == preview.total_bytes {
        return Ok(TailUpdate::Append(preview));
    }

    read_tail(service, profile, path).await
}

async fn read_tail(
    service: &SshService,
    profile: &SshProfile,
    path: &str,
) -> Result<TailUpdate, String> {
    let position = RemoteFileChunkPosition::Tail;
    let chunk = service
        .read_file_chunk(profile, path, position)
        .await
        .map_err(|error| error.to_string())?;
    decode_remote_file_chunk(chunk, position).map(TailUpdate::Replace)
}

fn is_log_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_files_follow_automatically() {
        assert!(enables_auto_refresh("/var/log/app.LOG"));
        assert!(supports_auto_refresh("/tmp/app.log", false));
        assert!(!enables_auto_refresh("/tmp/config.yml"));
    }

    #[test]
    fn any_windowed_text_can_opt_into_refresh() {
        assert!(supports_auto_refresh("/tmp/archive.txt", true));
        assert!(!supports_auto_refresh("/tmp/archive.txt", false));
    }
}
