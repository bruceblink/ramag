use ramag_domain::error::{DomainError, Result};

pub(crate) struct LimitedBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

pub(crate) fn read_limited(
    mut reader: impl std::io::Read,
    limit: usize,
) -> std::io::Result<LimitedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let keep = read.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..keep]);
        if keep < read {
            truncated = true;
        }
    }
    Ok(LimitedBytes { bytes, truncated })
}

pub(super) fn terminate_child(child: &mut std::process::Child) -> Result<()> {
    let running = child
        .try_wait()
        .map_err(|error| DomainError::QueryFailed(format!("检查 git 进程失败: {error}")))?
        .is_none();
    if running {
        child
            .kill()
            .map_err(|error| DomainError::QueryFailed(format!("终止 git 进程失败: {error}")))?;
    }
    child
        .wait()
        .map_err(|error| DomainError::QueryFailed(format!("回收 git 进程失败: {error}")))?;
    Ok(())
}

pub(super) fn wait_child_or_cleanup(
    child: &mut std::process::Child,
    operation: &str,
) -> Result<std::process::ExitStatus> {
    match child.wait() {
        Ok(status) => Ok(status),
        Err(error) => {
            // wait 失败后仍须终止并回收子进程，同时保留原始错误。
            let kill_error = child.kill().err();
            let reap_error = child.wait().err();
            let cleanup = match (kill_error, reap_error) {
                (None, None) => String::new(),
                (kill, reap) => format!(
                    "；清理失败：kill={}，wait={}",
                    kill.map_or_else(|| "ok".into(), |error| error.to_string()),
                    reap.map_or_else(|| "ok".into(), |error| error.to_string())
                ),
            };
            Err(DomainError::QueryFailed(format!(
                "等待 git {operation} 进程失败：{error}{cleanup}"
            )))
        }
    }
}
