//! OpenSSH SFTP 子进程、协议边界与会话缓存。

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex as ParkingMutex;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::rawsession::Limits;
use russh_sftp::client::{Config, RawSftpSession};
use russh_sftp::extensions;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use ramag_domain::entities::{SshProfile, SshProfileId, TRANSFER_BUFFER_BYTES};
use ramag_domain::error::{DomainError, Result};

use crate::askpass::AskPassBroker;
use crate::command::configure_no_window;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
const STDERR_LIMIT: usize = 16 * 1024;
const TEXT_PREAMBLE_LIMIT: usize = 16 * 1024;
const MAX_SFTP_PACKET_BYTES: u32 = 256 * 1024;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SftpTransport {
    Subsystem,
    WindowsRemoteServer,
}

#[derive(Clone)]
pub struct SessionCache {
    connections: Arc<Mutex<HashMap<SshProfileId, Arc<SftpConnection>>>>,
    connect_locks: Arc<Mutex<HashMap<SshProfileId, Arc<Mutex<()>>>>>,
    shutting_down: Arc<AtomicBool>,
    askpass: Arc<AskPassBroker>,
}

impl SessionCache {
    pub fn new(askpass: Arc<AskPassBroker>) -> Self {
        Self {
            connections: Arc::new(Mutex::new(HashMap::new())),
            connect_locks: Arc::new(Mutex::new(HashMap::new())),
            shutting_down: Arc::new(AtomicBool::new(false)),
            askpass,
        }
    }

    pub async fn get_or_connect(
        &self,
        profile: &SshProfile,
        program: &str,
        args: &[String],
        transport: SftpTransport,
    ) -> Result<Arc<SftpConnection>> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(DomainError::ConnectionFailed(
                "SSH 会话管理器正在关闭".into(),
            ));
        }
        let connect_lock = self.profile_connect_lock(&profile.id).await;
        let _connect_guard = connect_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(DomainError::ConnectionFailed(
                "SSH 会话管理器正在关闭".into(),
            ));
        }
        let stale = {
            let mut connections = self.connections.lock().await;
            match connections.get(&profile.id) {
                Some(connection) if connection.matches(profile, program, args, transport) => {
                    return Ok(connection.clone());
                }
                Some(_) => connections.remove(&profile.id),
                None => None,
            }
        };
        if let Some(stale) = stale {
            stale.close().await;
        }

        let environment = self.askpass.environment(profile)?;
        let created = Arc::new(
            SftpConnection::connect(profile, program, args, transport, &environment).await?,
        );
        let replaced = self
            .connections
            .lock()
            .await
            .insert(profile.id.clone(), created.clone());
        if let Some(replaced) = replaced {
            replaced.close().await;
        }
        Ok(created)
    }

    pub async fn invalidate(&self, profile_id: &SshProfileId) {
        let connect_lock = self.profile_connect_lock(profile_id).await;
        let _connect_guard = connect_lock.lock().await;
        if let Some(connection) = self.connections.lock().await.remove(profile_id) {
            connection.close().await;
        }
    }

    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let connect_locks = self
            .connect_locks
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut connect_guards = Vec::with_capacity(connect_locks.len());
        for connect_lock in connect_locks {
            connect_guards.push(connect_lock.lock_owned().await);
        }
        let connections = {
            let mut guard = self.connections.lock().await;
            guard
                .drain()
                .map(|(_, connection)| connection)
                .collect::<Vec<_>>()
        };
        for connection in connections {
            connection.close().await;
        }
        drop(connect_guards);
    }

    async fn profile_connect_lock(&self, profile_id: &SshProfileId) -> Arc<Mutex<()>> {
        self.connect_locks
            .lock()
            .await
            .entry(profile_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub struct SftpConnection {
    pub session: Arc<StructuredSftpSession>,
    profile: SshProfile,
    program: String,
    args: Vec<String>,
    transport: SftpTransport,
    child: Mutex<Option<Child>>,
    protocol_task: Mutex<Option<JoinHandle<()>>>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    stderr_tail: Arc<ParkingMutex<VecDeque<u8>>>,
}

/// 仅暴露项目需要的结构化 SFTP 能力，避免高级 `read_dir` 先无界收集整目录。
pub struct StructuredSftpSession {
    pub(crate) raw: Arc<RawSftpSession>,
    pub(crate) supports_fsync: bool,
    pub(crate) read_chunk_bytes: u32,
    pub(crate) write_chunk_bytes: usize,
}

impl StructuredSftpSession {
    pub fn close(&self) -> std::result::Result<(), SftpError> {
        self.raw.close_session()
    }

    pub async fn canonicalize(&self, path: String) -> std::result::Result<String, SftpError> {
        let names = self.raw.realpath(path).await?;
        names
            .files
            .first()
            .map(|file| file.filename.clone())
            .ok_or_else(|| SftpError::UnexpectedBehavior("realpath returned no path".into()))
    }
}

impl SftpConnection {
    async fn connect(
        profile: &SshProfile,
        program: &str,
        args: &[String],
        transport: SftpTransport,
        environment: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_no_window(command.as_std_mut());
        let mut child = command.spawn().map_err(|error| {
            DomainError::ConnectionFailed(format!("启动 OpenSSH SFTP 子进程失败：{error}"))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            DomainError::ConnectionFailed("OpenSSH SFTP 未创建 stdin 管道".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            DomainError::ConnectionFailed("OpenSSH SFTP 未创建 stdout 管道".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            DomainError::ConnectionFailed("OpenSSH SFTP 未创建 stderr 管道".into())
        })?;
        let stderr_tail = Arc::new(ParkingMutex::new(VecDeque::with_capacity(STDERR_LIMIT)));
        let stderr_task = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
        let (protocol_reader, protocol_writer) = tokio::io::duplex(MAX_SFTP_PACKET_BYTES as usize);
        let protocol_task = tokio::spawn(relay_bounded_packets(
            stdout,
            protocol_writer,
            transport == SftpTransport::WindowsRemoteServer,
        ));
        let stream = tokio::io::join(protocol_reader, stdin);

        let session = match timeout(CONNECT_TIMEOUT, initialize_session(stream)).await {
            Ok(Ok(session)) => session,
            Ok(Err(error)) => {
                stop_child(&mut child).await;
                let _ = timeout(Duration::from_secs(1), protocol_task).await;
                let _ = timeout(Duration::from_secs(1), stderr_task).await;
                return Err(connection_error(error.to_string(), &stderr_tail.lock()));
            }
            Err(_) => {
                stop_child(&mut child).await;
                let _ = timeout(Duration::from_secs(1), protocol_task).await;
                let _ = timeout(Duration::from_secs(1), stderr_task).await;
                return Err(connection_error(
                    "SFTP 协议握手在 15 秒内未完成".into(),
                    &stderr_tail.lock(),
                ));
            }
        };
        tracing::info!(
            profile_id = %profile.id,
            transport = ?transport,
            "ssh sftp session connected"
        );

        Ok(Self {
            session: Arc::new(session),
            profile: profile.clone(),
            program: program.into(),
            args: args.to_vec(),
            transport,
            child: Mutex::new(Some(child)),
            protocol_task: Mutex::new(Some(protocol_task)),
            stderr_task: Mutex::new(Some(stderr_task)),
            stderr_tail,
        })
    }

    fn matches(
        &self,
        profile: &SshProfile,
        program: &str,
        args: &[String],
        transport: SftpTransport,
    ) -> bool {
        self.profile == *profile
            && self.program == program
            && self.args == args
            && self.transport == transport
    }

    pub async fn close(&self) {
        if let Err(error) = self.session.close() {
            tracing::warn!(error = %error, "close ssh sftp protocol failed");
        }
        if let Some(mut child) = self.child.lock().await.take() {
            match timeout(CLOSE_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "wait ssh sftp child failed");
                }
                Err(_) => stop_child(&mut child).await,
            }
        }
        if let Some(task) = self.protocol_task.lock().await.take()
            && timeout(Duration::from_secs(1), task).await.is_err()
        {
            tracing::warn!("ssh sftp protocol relay did not stop in time");
        }
        if let Some(task) = self.stderr_task.lock().await.take()
            && timeout(Duration::from_secs(1), task).await.is_err()
        {
            tracing::warn!("ssh sftp stderr drain did not stop in time");
        }
        tracing::info!(profile_id = %self.profile.id, "ssh sftp session closed");
    }

    pub fn contextualize<T>(&self, result: Result<T>) -> Result<T> {
        let Err(DomainError::ConnectionFailed(message)) = result else {
            return result;
        };
        let hint = self.stderr_hint();
        if hint.is_empty() {
            Err(DomainError::ConnectionFailed(message))
        } else {
            Err(DomainError::ConnectionFailed(format!(
                "{message}；OpenSSH：{hint}"
            )))
        }
    }

    fn stderr_hint(&self) -> String {
        sanitized_tail(&self.stderr_tail.lock())
    }
}

async fn initialize_session<S>(stream: S) -> std::result::Result<StructuredSftpSession, SftpError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let config = Config {
        max_packet_len: MAX_SFTP_PACKET_BYTES,
        max_concurrent_writes: 1,
        request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
    };
    let mut raw = RawSftpSession::new_with_config(stream, config);
    let version = raw.init().await?;
    let supports_fsync = version
        .extensions
        .get(extensions::FSYNC)
        .is_some_and(|version| version == "1");
    let limits = if version
        .extensions
        .get(extensions::LIMITS)
        .is_some_and(|version| version == "1")
    {
        let limits = Limits::from(raw.limits().await?);
        raw.set_limits(limits);
        Some(limits)
    } else {
        None
    };
    let read_chunk_bytes = bounded_chunk(limits.and_then(|value| value.read_len));
    let write_chunk_bytes = bounded_chunk(limits.and_then(|value| value.write_len)) as usize;
    Ok(StructuredSftpSession {
        raw: Arc::new(raw),
        supports_fsync,
        read_chunk_bytes,
        write_chunk_bytes,
    })
}

fn bounded_chunk(server_limit: Option<u64>) -> u32 {
    server_limit
        .unwrap_or(TRANSFER_BUFFER_BYTES as u64)
        .min(TRANSFER_BUFFER_BYTES as u64)
        .clamp(1, u64::from(u32::MAX)) as u32
}

async fn relay_bounded_packets<R, W>(mut stdout: R, mut destination: W, allow_text_preamble: bool)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let Some((header, first_body_byte)) =
        read_first_packet_header(&mut stdout, allow_text_preamble).await
    else {
        let _ = destination.shutdown().await;
        return;
    };
    let mut buffer = [0u8; 32 * 1024];
    if let Err(error) = relay_packet(
        &mut stdout,
        &mut destination,
        header,
        first_body_byte,
        &mut buffer,
    )
    .await
    {
        tracing::warn!(error = %error, "relay ssh sftp first packet failed");
        let _ = destination.shutdown().await;
        return;
    }
    loop {
        let mut header = [0u8; 4];
        match stdout.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                tracing::warn!(error = %error, "read ssh sftp packet header failed");
                break;
            }
        }
        if let Err(error) =
            relay_packet(&mut stdout, &mut destination, header, None, &mut buffer).await
        {
            tracing::warn!(error = %error, "relay ssh sftp packet failed");
            break;
        }
    }
    if let Err(error) = destination.shutdown().await {
        tracing::warn!(error = %error, "shutdown ssh sftp protocol relay failed");
    }
}

async fn read_first_packet_header<R>(
    stdout: &mut R,
    allow_text_preamble: bool,
) -> Option<([u8; 4], Option<u8>)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    if !allow_text_preamble {
        let mut header = [0u8; 4];
        return stdout
            .read_exact(&mut header)
            .await
            .ok()
            .map(|_| (header, None));
    }
    let mut scanned = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while scanned.len() < TEXT_PREAMBLE_LIMIT {
        if let Err(error) = stdout.read_exact(&mut byte).await {
            if error.kind() != std::io::ErrorKind::UnexpectedEof {
                tracing::warn!(error = %error, "read ssh sftp first packet failed");
            }
            return None;
        }
        scanned.push(byte[0]);
        if scanned.len() < 5 {
            continue;
        }
        let start = scanned.len() - 5;
        let header: [u8; 4] = scanned[start..start + 4].try_into().ok()?;
        let packet_bytes = u32::from_be_bytes(header);
        // 首包必须是 SSH_FXP_VERSION（类型 2），且至少含类型和版本号。
        if (5..=MAX_SFTP_PACKET_BYTES).contains(&packet_bytes) && scanned[start + 4] == 2 {
            if start > 0 {
                tracing::info!(bytes = start, "ignored jumpserver sftp text preamble");
            }
            return Some((header, Some(2)));
        }
    }
    tracing::warn!("jumpserver sftp text preamble exceeded safety limit");
    None
}

async fn relay_packet<R, W>(
    stdout: &mut R,
    destination: &mut W,
    header: [u8; 4],
    first_body_byte: Option<u8>,
    buffer: &mut [u8],
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let packet_bytes = u32::from_be_bytes(header);
    if packet_bytes == 0 || packet_bytes > MAX_SFTP_PACKET_BYTES {
        tracing::warn!(packet_bytes, "ssh sftp packet exceeded safety limit");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid sftp packet size",
        ));
    }
    destination.write_all(&header).await?;
    let mut remaining = packet_bytes as usize;
    if let Some(first) = first_body_byte {
        destination.write_all(&[first]).await?;
        remaining = remaining.saturating_sub(1);
    }
    while remaining > 0 {
        let chunk = remaining.min(buffer.len());
        stdout.read_exact(&mut buffer[..chunk]).await?;
        destination.write_all(&buffer[..chunk]).await?;
        remaining -= chunk;
    }
    Ok(())
}

async fn drain_stderr(
    mut stderr: tokio::process::ChildStderr,
    tail: Arc<ParkingMutex<VecDeque<u8>>>,
) {
    let mut buffer = [0u8; 4096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => append_tail(&mut tail.lock(), &buffer[..read]),
            Err(error) => {
                tracing::warn!(error = %error, "drain ssh sftp stderr failed");
                break;
            }
        }
    }
}

fn append_tail(tail: &mut VecDeque<u8>, bytes: &[u8]) {
    let overflow = tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(STDERR_LIMIT);
    tail.drain(..overflow.min(tail.len()));
    let start = bytes.len().saturating_sub(STDERR_LIMIT);
    tail.extend(&bytes[start..]);
}

fn sanitized_tail(tail: &VecDeque<u8>) -> String {
    String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<_>>())
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>()
        .trim()
        .to_string()
}

fn connection_error(protocol_error: String, stderr: &VecDeque<u8>) -> DomainError {
    let detail = sanitized_tail(stderr);
    let lower = detail.to_ascii_lowercase();
    let hint = if lower.contains("host key verification failed")
        || lower.contains("no host key is known")
        || lower.contains("remote host identification has changed")
    {
        "主机指纹尚未受信任或已变化。请先通过受信任渠道核对指纹并写入 known_hosts，再重试连接。"
            .into()
    } else if lower.contains("permission denied")
        || lower.contains("password")
        || lower.contains("passphrase")
    {
        "SSH 认证失败，请检查用户名、密码、密钥或 Agent 配置。加密密钥请先在 SSH Agent 中解锁。"
            .into()
    } else if detail.is_empty() {
        protocol_error
    } else {
        format!("{protocol_error}；OpenSSH：{detail}")
    };
    DomainError::ConnectionFailed(hint)
}

async fn stop_child(child: &mut Child) {
    if let Err(error) = child.start_kill() {
        tracing::warn!(error = %error, "kill ssh sftp child failed");
    }
    if timeout(CLOSE_TIMEOUT, child.wait()).await.is_err() {
        tracing::warn!("ssh sftp child did not exit after kill");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_tail_is_bounded() {
        let mut tail = VecDeque::new();
        append_tail(&mut tail, &vec![b'a'; STDERR_LIMIT + 100]);
        assert_eq!(tail.len(), STDERR_LIMIT);
    }

    #[test]
    fn connection_errors_explain_auth_and_host_key_limits() {
        let host_key = VecDeque::from(b"Host key verification failed".to_vec());
        assert!(
            connection_error("bad".into(), &host_key)
                .message()
                .contains("known_hosts")
        );
        let auth = VecDeque::from(b"Permission denied (publickey,password)".to_vec());
        assert!(
            connection_error("bad".into(), &auth)
                .message()
                .contains("认证失败")
        );
    }

    #[test]
    fn transfer_chunks_respect_server_and_local_limits() {
        assert_eq!(bounded_chunk(None), 64 * 1024);
        assert_eq!(bounded_chunk(Some(4096)), 4096);
        assert_eq!(bounded_chunk(Some(0)), 1);
        assert_eq!(bounded_chunk(Some(u64::MAX)), 64 * 1024);
    }

    #[tokio::test]
    async fn packet_relay_forwards_valid_frame_and_rejects_oversized_frame() {
        let (mut input, input_reader) = tokio::io::duplex(64);
        let (mut output_reader, output) = tokio::io::duplex(64);
        let relay = tokio::spawn(relay_bounded_packets(input_reader, output, false));
        input.write_all(&3u32.to_be_bytes()).await.unwrap();
        input.write_all(&[1, 2, 3]).await.unwrap();
        input.shutdown().await.unwrap();
        let mut forwarded = Vec::new();
        output_reader.read_to_end(&mut forwarded).await.unwrap();
        relay.await.unwrap();
        assert_eq!(forwarded, [0, 0, 0, 3, 1, 2, 3]);

        let (mut input, input_reader) = tokio::io::duplex(64);
        let (mut output_reader, output) = tokio::io::duplex(64);
        let relay = tokio::spawn(relay_bounded_packets(input_reader, output, false));
        input
            .write_all(&(MAX_SFTP_PACKET_BYTES + 1).to_be_bytes())
            .await
            .unwrap();
        input.shutdown().await.unwrap();
        let mut forwarded = Vec::new();
        output_reader.read_to_end(&mut forwarded).await.unwrap();
        relay.await.unwrap();
        assert!(forwarded.is_empty());
    }

    #[tokio::test]
    async fn packet_relay_skips_a_bounded_jumpserver_banner_before_version() {
        let (mut input, input_reader) = tokio::io::duplex(256);
        let (mut output_reader, output) = tokio::io::duplex(256);
        let relay = tokio::spawn(relay_bounded_packets(input_reader, output, true));
        input
            .write_all(b"Welcome to JumpServer SSH Server\r\n")
            .await
            .unwrap();
        input.write_all(&5u32.to_be_bytes()).await.unwrap();
        input.write_all(&[2, 0, 0, 0, 3]).await.unwrap();
        input.shutdown().await.unwrap();
        let mut forwarded = Vec::new();
        output_reader.read_to_end(&mut forwarded).await.unwrap();
        relay.await.unwrap();

        assert_eq!(forwarded, [0, 0, 0, 5, 2, 0, 0, 0, 3]);
    }
}
