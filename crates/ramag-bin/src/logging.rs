//! 应用日志初始化：首选用户数据目录，失败时回退临时目录。

use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// 运行期按大小轮转；仅启动时检查文件大小会让长期托盘进程的日志继续无界增长。
struct RotatingLogWriter {
    path: PathBuf,
    file: Option<std::fs::File>,
    written: u64,
    max_bytes: u64,
}

impl RotatingLogWriter {
    fn new(path: PathBuf, file: std::fs::File, max_bytes: u64) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log size limit must be positive",
            ));
        }
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            written,
            max_bytes,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        // Windows 不能重命名仍被当前进程打开的文件，必须先释放句柄。
        self.file.take();
        match rotate_log_file(&self.path) {
            Ok(()) => {
                self.file = Some(open_log_handle(&self.path, false)?);
            }
            Err(rotate_error) => {
                // 备份文件被占用等情况下仍要守住磁盘上限；退化为截断当前日志。
                let file = open_log_handle(&self.path, true).map_err(|truncate_error| {
                    io::Error::other(format!(
                        "rotate log failed: {rotate_error}; truncate fallback failed: {truncate_error}"
                    ))
                })?;
                eprintln!("ramag log rotation failed; truncated current log: {rotate_error}");
                self.file = Some(file);
            }
        }
        self.written = 0;
        Ok(())
    }

    fn file_mut(&mut self) -> io::Result<&mut std::fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is unavailable"))
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let consumed = buf.len();
        while !buf.is_empty() {
            if self.written >= self.max_bytes {
                self.rotate()?;
            }
            let remaining = usize::try_from(self.max_bytes - self.written)
                .unwrap_or(usize::MAX)
                .min(buf.len());
            self.file_mut()?.write_all(&buf[..remaining])?;
            self.written = self.written.saturating_add(remaining as u64);
            buf = &buf[remaining..];
        }
        Ok(consumed)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file_mut()?.flush()
    }
}

pub(crate) fn init() -> Option<PathBuf> {
    let (filter, filter_error) = match std::env::var("RUST_LOG") {
        Ok(value) => match EnvFilter::try_new(value) {
            Ok(filter) => (filter, None),
            Err(error) => (EnvFilter::new("info"), Some(error.to_string())),
        },
        Err(std::env::VarError::NotPresent) => (EnvFilter::new("info"), None),
        Err(error) => (EnvFilter::new("info"), Some(error.to_string())),
    };
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true);

    let (log_path, log_file, fallback_error) = open_log_file();
    let has_log_file = log_file.is_some();
    let file_layer = log_file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_ansi(false)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    install_panic_hook();

    if let Some(error) = fallback_error {
        error!(
            operation = "logging_file_init",
            error = %error,
            "preferred log file unavailable"
        );
    }
    if let Some(error) = filter_error {
        error!(operation = "logging_filter_init", error = %error, "invalid RUST_LOG filter; using info");
    }
    if has_log_file {
        info!(operation = "logging_file_init", log = %log_path.display(), "log file ready");
        Some(log_path)
    } else {
        error!(
            operation = "logging_file_init",
            reason = "unavailable",
            "no writable log file available"
        );
        None
    }
}

/// Windows Release 没有控制台；保留默认 panic 输出的同时，把未捕获 panic 写入日志文件。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let location = panic.location();
        error!(
            operation = "application_panic",
            panic = %panic,
            file = location.map_or("unknown", std::panic::Location::file),
            line = location.map_or(0, std::panic::Location::line),
            column = location.map_or(0, std::panic::Location::column),
            thread = ?std::thread::current().id(),
            "unhandled panic"
        );
        previous(panic);
    }));
}

fn open_log_file() -> (PathBuf, Option<RotatingLogWriter>, Option<String>) {
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
) -> (PathBuf, Option<RotatingLogWriter>, Option<String>) {
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

fn try_open_log_file(dir: &Path) -> io::Result<(PathBuf, RotatingLogWriter)> {
    try_open_log_file_at(dir, MAX_LOG_BYTES)
}

fn try_open_log_file_at(dir: &Path, max_bytes: u64) -> io::Result<(PathBuf, RotatingLogWriter)> {
    if max_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log size limit must be positive",
        ));
    }
    std::fs::create_dir_all(dir)?;
    reject_symlink(dir, "log directory")?;
    set_private_dir_permissions(dir)?;
    let path = dir.join("ramag.log");
    reject_symlink(&path, "log file")?;
    reject_symlink(&path.with_extension("log.old"), "log backup")?;
    rotate_if_oversized_at(&path, max_bytes)?;
    let file = open_log_handle(&path, false)?;
    let writer = RotatingLogWriter::new(path.clone(), file, max_bytes)?;
    Ok((path, writer))
}

fn reject_symlink(path: &Path, kind: &str) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{kind} must not be a symbolic link: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_log_handle(path: &Path, truncate: bool) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn rotate_if_oversized_at(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    if max_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log size limit must be positive",
        ));
    }
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return retain_log_tail(&path.with_extension("log.old"), max_bytes);
        }
        Err(error) => return Err(error),
    };
    if metadata.len() < max_bytes {
        return retain_log_tail(&path.with_extension("log.old"), max_bytes);
    }
    rotate_log_file(path)?;
    retain_log_tail(&path.with_extension("log.old"), max_bytes)
}

/// 启动时可能接手旧版本留下的超大日志；轮转后只保留有诊断价值的最新尾部。
fn retain_log_tail(path: &Path, max_bytes: u64) -> io::Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if len <= max_bytes {
        return Ok(());
    }
    let capacity = usize::try_from(max_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "log size limit exceeds addressable memory",
        )
    })?;
    let mut source = std::fs::File::open(path)?;
    source.seek(io::SeekFrom::Start(len - max_bytes))?;
    let mut tail = Vec::new();
    tail.try_reserve_exact(capacity)
        .map_err(|error| io::Error::other(format!("reserve log tail buffer failed: {error}")))?;
    (&mut source).take(max_bytes).read_to_end(&mut tail)?;
    drop(source);

    let mut target = open_log_handle(path, true)?;
    target.write_all(&tail)?;
    target.flush()
}

fn rotate_log_file(path: &Path) -> io::Result<()> {
    if !path.exists() {
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
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{rotate_if_oversized_at, try_open_log_file, try_open_log_file_at};

    #[test]
    fn oversized_log_backup_keeps_bounded_tail() -> std::io::Result<()> {
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
        assert_eq!(std::fs::read(path.with_extension("log.old"))?, b"2345");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn zero_log_limit_does_not_modify_existing_log() -> std::io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ramag-log-limit-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("ramag.log");
        std::fs::write(&path, b"keep")?;

        assert!(try_open_log_file_at(&dir, 0).is_err());
        assert_eq!(std::fs::read(&path)?, b"keep");
        assert!(!path.with_extension("log.old").exists());
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn stale_backup_is_bounded_even_when_current_log_is_missing() -> std::io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ramag-stale-log-backup-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("ramag.log");
        let backup = path.with_extension("log.old");
        std::fs::write(&backup, b"12345")?;

        rotate_if_oversized_at(&path, 4)?;

        assert_eq!(std::fs::read(&backup)?, b"2345");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn long_running_writer_rotates_without_restart() -> std::io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ramag-runtime-log-rotation-test-{}-{nonce}",
            std::process::id()
        ));
        let (path, mut writer) = try_open_log_file_at(&dir, 4)?;

        writer.write_all(b"1234")?;
        writer.flush()?;
        writer.write_all(b"5678")?;
        writer.flush()?;

        assert_eq!(std::fs::read(path.with_extension("log.old"))?, b"1234");
        assert_eq!(std::fs::read(&path)?, b"5678");
        drop(writer);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn log_directory_and_file_are_private() -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ramag-log-permission-test-{}-{nonce}",
            std::process::id()
        ));
        let (path, file) = try_open_log_file(&dir)?;
        drop(file);
        assert_eq!(std::fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_log_file_is_rejected() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ramag-log-symlink-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let target = dir.join("target");
        std::fs::write(&target, b"keep")?;
        symlink(&target, dir.join("ramag.log"))?;

        assert!(try_open_log_file(&dir).is_err());
        assert_eq!(std::fs::read(&target)?, b"keep");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_log_directory_is_rejected() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ramag-log-dir-symlink-test-{}-{nonce}",
            std::process::id()
        ));
        let target = root.join("target");
        std::fs::create_dir_all(&target)?;
        let link = root.join("logs");
        symlink(&target, &link)?;

        assert!(try_open_log_file(&link).is_err());
        assert!(!target.join("ramag.log").exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
