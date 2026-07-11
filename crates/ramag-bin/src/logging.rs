//! 应用日志初始化：首选用户数据目录，失败时回退临时目录。

use std::path::{Path, PathBuf};

use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

pub(crate) fn init() -> Option<PathBuf> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);

    let (log_path, log_file, fallback_error) = open_log_file();
    let has_log_file = log_file.is_some();
    let file_layer = log_file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_target(false)
            .with_ansi(false)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    install_panic_hook();

    if let Some(error) = fallback_error {
        error!(%error, "preferred log file unavailable");
    }
    if has_log_file {
        eprintln!("ramag log file: {}", log_path.display());
        info!(log = %log_path.display(), "log file ready");
        Some(log_path)
    } else {
        error!("no writable log file available");
        None
    }
}

/// Windows Release 没有控制台；保留默认 panic 输出的同时，把未捕获 panic 写入日志文件。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        error!(panic = %panic, "unhandled panic");
        previous(panic);
    }));
}

fn open_log_file() -> (PathBuf, Option<std::fs::File>, Option<String>) {
    let preferred_dir = directories::ProjectDirs::from("com", "ramag", "ramag")
        .map(|project| project.data_dir().join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("ramag-logs"));
    match try_open_log_file(&preferred_dir) {
        Ok((path, file)) => (path, Some(file), None),
        Err(preferred_error) => open_fallback_log(preferred_dir, preferred_error),
    }
}

fn open_fallback_log(
    preferred_dir: PathBuf,
    preferred_error: std::io::Error,
) -> (PathBuf, Option<std::fs::File>, Option<String>) {
    let fallback_dir = std::env::temp_dir().join("ramag-logs");
    match try_open_log_file(&fallback_dir) {
        Ok((path, file)) => (
            path,
            Some(file),
            Some(format!(
                "无法使用首选日志目录 {}：{preferred_error}",
                preferred_dir.display()
            )),
        ),
        Err(fallback_error) => (
            preferred_dir.join("ramag.log"),
            None,
            Some(format!(
                "首选日志失败：{preferred_error}；临时日志失败：{fallback_error}"
            )),
        ),
    }
}

fn try_open_log_file(dir: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("ramag.log");
    rotate_if_oversized(&path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    Ok((path, file))
}

fn rotate_if_oversized(path: &Path) -> std::io::Result<()> {
    rotate_if_oversized_at(path, MAX_LOG_BYTES)
}

fn rotate_if_oversized_at(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.len() < max_bytes {
        return Ok(());
    }
    let backup = path.with_extension("log.old");
    match std::fs::remove_file(&backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(path, backup)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::rotate_if_oversized_at;

    #[test]
    fn oversized_log_is_rotated_once() -> std::io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ramag-log-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("ramag.log");
        std::fs::write(&path, b"12345")?;
        rotate_if_oversized_at(&path, 4)?;
        assert!(!path.exists());
        assert_eq!(std::fs::read(path.with_extension("log.old"))?, b"12345");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
