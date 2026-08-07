//! Linux 单实例：在用户私有的 XDG runtime 目录监听 Unix socket。
//! 后启进程连接 socket 请求首实例唤起；失效 socket 仅在确认无法连接后清理。

use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::FileTypeExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{info, warn};

const SOCKET_NAME: &str = "ramag.sock";
const ACTIVATE_MESSAGE: &[u8] = b"activate\n";

pub(crate) enum InstanceRole {
    Primary(PrimaryGuard),
    Secondary,
}

pub(crate) struct PrimaryGuard {
    path: Option<PathBuf>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rx: Option<Receiver<()>>,
}

impl PrimaryGuard {
    pub(crate) fn poll_activate(&self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut fired = false;
        while rx.try_recv().is_ok() {
            fired = true;
        }
        fired
    }

    fn degraded() -> Self {
        Self {
            path: None,
            stopping: Arc::new(AtomicBool::new(false)),
            thread: None,
            rx: None,
        }
    }
}

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(path) = &self.path {
            let _ = UnixStream::connect(path);
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            warn!("single-instance listener thread panicked");
        }
        if let Some(path) = &self.path
            && let Err(error) = remove_socket(path)
        {
            warn!(error = %error, path = %path.display(), "remove single-instance socket failed");
        }
    }
}

pub(crate) fn acquire() -> InstanceRole {
    let Some(path) = runtime_socket_path() else {
        warn!("XDG_RUNTIME_DIR unavailable; single-instance protection disabled");
        return InstanceRole::Primary(PrimaryGuard::degraded());
    };
    acquire_at(path)
}

fn runtime_socket_path() -> Option<PathBuf> {
    let directory = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    directory.is_absolute().then(|| directory.join(SOCKET_NAME))
}

fn acquire_at(path: PathBuf) -> InstanceRole {
    match UnixListener::bind(&path) {
        Ok(listener) => start_primary(path, listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if notify_primary(&path) {
                return InstanceRole::Secondary;
            }
            match remove_stale_socket(&path).and_then(|()| UnixListener::bind(&path)) {
                Ok(listener) => start_primary(path, listener),
                Err(retry_error) => {
                    warn!(
                        error = %retry_error,
                        path = %path.display(),
                        "recover stale single-instance socket failed; protection disabled"
                    );
                    InstanceRole::Primary(PrimaryGuard::degraded())
                }
            }
        }
        Err(error) => {
            warn!(error = %error, path = %path.display(), "bind single-instance socket failed; protection disabled");
            InstanceRole::Primary(PrimaryGuard::degraded())
        }
    }
}

fn start_primary(path: PathBuf, listener: UnixListener) -> InstanceRole {
    let stopping = Arc::new(AtomicBool::new(false));
    let thread_stopping = stopping.clone();
    let (tx, rx) = sync_channel::<()>(1);
    let thread = std::thread::Builder::new()
        .name("ramag-single-instance".into())
        .spawn(move || {
            for connection in listener.incoming() {
                if thread_stopping.load(Ordering::Acquire) {
                    break;
                }
                let mut stream = match connection {
                    Ok(stream) => stream,
                    Err(error) => {
                        warn!(error = %error, "accept single-instance activation failed");
                        break;
                    }
                };
                if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(1))) {
                    warn!(error = %error, "set single-instance socket timeout failed");
                    continue;
                }
                let mut message = [0_u8; ACTIVATE_MESSAGE.len()];
                if stream.read_exact(&mut message).is_ok() && message == ACTIVATE_MESSAGE {
                    match tx.try_send(()) {
                        Ok(()) | Err(TrySendError::Full(())) => {}
                        Err(TrySendError::Disconnected(())) => break,
                    }
                }
            }
        });

    match thread {
        Ok(thread) => InstanceRole::Primary(PrimaryGuard {
            path: Some(path),
            stopping,
            thread: Some(thread),
            rx: Some(rx),
        }),
        Err(error) => {
            warn!(error = %error, "start single-instance listener failed; protection disabled");
            if let Err(remove_error) = remove_socket(&path) {
                warn!(error = %remove_error, path = %path.display(), "remove unused single-instance socket failed");
            }
            InstanceRole::Primary(PrimaryGuard::degraded())
        }
    }
}

fn notify_primary(path: &Path) -> bool {
    match UnixStream::connect(path).and_then(|mut stream| stream.write_all(ACTIVATE_MESSAGE)) {
        Ok(()) => {
            info!("existing instance notified to reveal its window");
            true
        }
        Err(error) => {
            warn!(error = %error, path = %path.display(), "notify existing instance failed");
            false
        }
    }
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "single-instance path exists and is not a socket",
        ));
    }
    remove_socket(path)
}

fn remove_socket(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_socket(name: &str) -> io::Result<(PathBuf, PathBuf)> {
        let directory = std::env::temp_dir().join(format!(
            "ramag-single-instance-{}-{name}",
            std::process::id()
        ));
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir(&directory)?;
        Ok((directory.join(SOCKET_NAME), directory))
    }

    #[test]
    fn second_instance_notifies_primary() -> io::Result<()> {
        let (path, directory) = test_socket("notify")?;
        let primary = match acquire_at(path) {
            InstanceRole::Primary(guard) => guard,
            InstanceRole::Secondary => {
                return Err(io::Error::other("first instance was secondary"));
            }
        };
        assert!(matches!(
            acquire_at(directory.join(SOCKET_NAME)),
            InstanceRole::Secondary
        ));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut activated = false;
        while std::time::Instant::now() < deadline {
            if primary.poll_activate() {
                activated = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(activated);
        drop(primary);
        std::fs::remove_dir(directory)
    }

    #[test]
    fn stale_socket_is_replaced() -> io::Result<()> {
        let (path, directory) = test_socket("stale")?;
        drop(UnixListener::bind(&path)?);
        let primary = match acquire_at(path) {
            InstanceRole::Primary(guard) => guard,
            InstanceRole::Secondary => return Err(io::Error::other("stale socket looked active")),
        };
        drop(primary);
        std::fs::remove_dir(directory)
    }
}
