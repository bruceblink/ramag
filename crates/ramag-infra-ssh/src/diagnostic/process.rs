//! 安全诊断 SSH 子进程的资源边界和生命周期管理。

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::sync::mpsc;

use ramag_domain::entities::{
    DiagnosticCancellation, DiagnosticTermination, MAX_DIAGNOSTIC_INPUT_BYTES,
    MAX_DIAGNOSTIC_OUTPUT_BYTES, MAX_DIAGNOSTIC_STDERR_BYTES, MAX_DIAGNOSTIC_TIMEOUT_SECONDS,
};
use ramag_domain::error::{DomainError, Result};

use crate::command::configure_no_window;

use super::bounded_visible_output;

pub(super) struct ProcessExecution {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) exit_code: Option<i32>,
    pub(super) termination: DiagnosticTermination,
    pub(super) truncated: bool,
}

impl ProcessExecution {
    pub(super) fn bounded_error(&self) -> String {
        let (output, _) = bounded_visible_output(&self.stdout, &self.stderr);
        if output.is_empty() {
            format!("远端进程退出状态 {:?}", self.exit_code)
        } else {
            output.chars().take(512).collect()
        }
    }
}

pub(super) async fn execute_process(
    executable: &str,
    args: Vec<String>,
    env: HashMap<String, String>,
    input: Vec<u8>,
    timeout: Duration,
    cancellation: DiagnosticCancellation,
) -> Result<ProcessExecution> {
    if input.len() > MAX_DIAGNOSTIC_INPUT_BYTES {
        return Err(DomainError::InvalidConfig(
            "结构化诊断请求超过输入上限".into(),
        ));
    }
    let timeout = timeout.min(Duration::from_secs(MAX_DIAGNOSTIC_TIMEOUT_SECONDS));
    let mut command = Command::new(executable);
    command
        .args(args)
        .envs(env)
        .stdin(if input.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_no_window(command.as_std_mut());
    let mut child = command.spawn().map_err(|error| {
        DomainError::ConnectionFailed(format!("启动安全诊断 SSH 失败：{error}"))
    })?;
    if !input.is_empty() {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| DomainError::Other("安全诊断 SSH 缺少标准输入管道".into()))?;
        stdin
            .write_all(&input)
            .await
            .map_err(|error| DomainError::ConnectionFailed(format!("发送诊断请求失败：{error}")))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| DomainError::ConnectionFailed(format!("关闭诊断输入失败：{error}")))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DomainError::Other("安全诊断 SSH 缺少标准输出管道".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DomainError::Other("安全诊断 SSH 缺少错误输出管道".into()))?;
    let (limit_tx, mut limit_rx) = mpsc::channel(2);
    let stdout_task = tokio::spawn(read_bounded(
        stdout,
        MAX_DIAGNOSTIC_OUTPUT_BYTES,
        limit_tx.clone(),
    ));
    let stderr_task = tokio::spawn(read_bounded(stderr, MAX_DIAGNOSTIC_STDERR_BYTES, limit_tx));
    let wait_cancelled = async {
        while !cancellation.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    tokio::pin!(wait_cancelled);
    let mut termination = DiagnosticTermination::Completed;
    let status = tokio::select! {
        status = child.wait() => Some(status.map_err(|error| {
            DomainError::ConnectionFailed(format!("等待安全诊断 SSH 失败：{error}"))
        })?),
        _ = tokio::time::sleep(timeout) => {
            termination = DiagnosticTermination::TimedOut;
            terminate_child(&mut child).await?;
            None
        }
        _ = &mut wait_cancelled => {
            termination = DiagnosticTermination::Cancelled;
            terminate_child(&mut child).await?;
            None
        }
        Some(()) = limit_rx.recv() => {
            termination = DiagnosticTermination::OutputLimitExceeded;
            terminate_child(&mut child).await?;
            None
        }
    };
    let stdout = join_reader(stdout_task, "stdout").await?;
    let stderr = join_reader(stderr_task, "stderr").await?;
    let truncated = stdout.exceeded
        || stderr.exceeded
        || termination == DiagnosticTermination::OutputLimitExceeded;
    if truncated && termination == DiagnosticTermination::Completed {
        termination = DiagnosticTermination::OutputLimitExceeded;
    }
    Ok(ProcessExecution {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        exit_code: status.and_then(|status| status.code()),
        termination,
        truncated,
    })
}

async fn terminate_child(child: &mut tokio::process::Child) -> Result<()> {
    child.kill().await.map_err(|error| {
        DomainError::ConnectionFailed(format!("终止安全诊断 SSH 失败：{error}"))
    })?;
    if let Err(error) = child.wait().await {
        tracing::warn!(operation = "ssh_diagnostic_cleanup", error = %error, "wait terminated ssh diagnostic process failed");
    }
    Ok(())
}

struct BoundedRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    limit_tx: mpsc::Sender<()>,
) -> std::io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(BoundedRead {
                bytes,
                exceeded: false,
            });
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining {
            let _ = limit_tx.send(()).await;
            return Ok(BoundedRead {
                bytes,
                exceeded: true,
            });
        }
    }
}

async fn join_reader(
    task: tokio::task::JoinHandle<std::io::Result<BoundedRead>>,
    stream: &str,
) -> Result<BoundedRead> {
    task.await
        .map_err(|error| DomainError::Other(format!("诊断 {stream} 读取任务异常退出：{error}")))?
        .map_err(|error| DomainError::ConnectionFailed(format!("读取诊断 {stream} 失败：{error}")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_output_still_waits_for_process_exit() {
        let execution = execute_process(
            "/bin/sh",
            vec!["-c".into(), "exec 1>&- 2>&-; sleep 0.05; exit 7".into()],
            HashMap::new(),
            Vec::new(),
            Duration::from_secs(1),
            DiagnosticCancellation::default(),
        )
        .await
        .unwrap();

        assert_eq!(execution.exit_code, Some(7));
        assert_eq!(execution.termination, DiagnosticTermination::Completed);
        assert!(!execution.truncated);
    }
}
