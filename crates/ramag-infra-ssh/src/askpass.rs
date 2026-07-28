//! OpenSSH AskPass 一次性凭据通道；密码不进入参数、环境变量或临时文件。

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;
use zeroize::Zeroize as _;

use ramag_domain::entities::{MAX_SSH_PASSWORD_BYTES, SshAuthMode, SshProfile};
use ramag_domain::error::{DomainError, Result};

const MARKER_ENV: &str = "RAMAG_SSH_ASKPASS";
const ENDPOINT_ENV: &str = "RAMAG_SSH_ASKPASS_ENDPOINT";
const TOKEN_ENV: &str = "RAMAG_SSH_ASKPASS_TOKEN";
const TOKEN_BYTES: usize = 32;
const MAX_PENDING_SECRETS: usize = 256;
const SECRET_TTL: Duration = Duration::from_secs(120);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROMPT_BYTES: usize = 4096;

pub(crate) struct AskPassBroker {
    server: Mutex<Option<BrokerServer>>,
}

impl AskPassBroker {
    pub(crate) fn new() -> Self {
        Self {
            server: Mutex::new(None),
        }
    }

    pub(crate) fn environment(&self, profile: &SshProfile) -> Result<HashMap<String, String>> {
        if profile.auth_mode != SshAuthMode::Password {
            return Ok(HashMap::new());
        }
        let executable = std::env::current_exe()
            .map_err(|error| DomainError::Other(format!("定位 Ramag AskPass 程序失败：{error}")))?;
        let executable = executable.to_str().ok_or_else(|| {
            DomainError::InvalidConfig("Ramag 程序路径不是有效 UTF-8，无法启用密码认证".into())
        })?;

        let mut server = self.server.lock();
        if server.is_none() {
            *server = Some(BrokerServer::start().map_err(|error| {
                DomainError::ConnectionFailed(format!("启动 SSH 密码凭据通道失败：{error}"))
            })?);
        }
        let server = server
            .as_ref()
            .ok_or_else(|| DomainError::Other("SSH 密码凭据通道初始化后不可用".into()))?;
        let token = server.register(profile.password.as_bytes())?;

        Ok(HashMap::from([
            ("SSH_ASKPASS".into(), executable.into()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (MARKER_ENV.into(), "1".into()),
            (ENDPOINT_ENV.into(), server.address.to_string()),
            (TOKEN_ENV.into(), token),
        ]))
    }

    pub(crate) fn clear(&self) {
        if let Some(server) = self.server.lock().as_ref() {
            server.shared.secrets.lock().clear();
        }
    }
}

impl Default for AskPassBroker {
    fn default() -> Self {
        Self::new()
    }
}

struct BrokerServer {
    address: SocketAddr,
    shared: Arc<BrokerShared>,
    thread: Option<JoinHandle<()>>,
}

impl BrokerServer {
    fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let shared = Arc::new(BrokerShared {
            secrets: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
        });
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("ramag-ssh-askpass".into())
            .spawn(move || serve(listener, &thread_shared))?;
        Ok(Self {
            address,
            shared,
            thread: Some(thread),
        })
    }

    fn register(&self, password: &[u8]) -> Result<String> {
        let mut secrets = self.shared.secrets.lock();
        let now = Instant::now();
        secrets.retain(|_, secret| secret.expires_at > now);
        if secrets.len() >= MAX_PENDING_SECRETS {
            return Err(DomainError::Other(
                "等待认证的 SSH 密码请求过多，请稍后重试".into(),
            ));
        }
        let token = loop {
            let candidate = Uuid::new_v4().simple().to_string();
            if !secrets.contains_key(&candidate) {
                break candidate;
            }
        };
        secrets.insert(
            token.clone(),
            PendingSecret {
                bytes: password.to_vec(),
                expires_at: now + SECRET_TTL,
            },
        );
        Ok(token)
    }
}

impl Drop for BrokerServer {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, IO_TIMEOUT);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("ssh askpass broker thread did not stop cleanly");
        }
        self.shared.secrets.lock().clear();
    }
}

struct BrokerShared {
    secrets: Mutex<HashMap<String, PendingSecret>>,
    stopping: AtomicBool,
}

struct PendingSecret {
    bytes: Vec<u8>,
    expires_at: Instant,
}

impl Drop for PendingSecret {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

fn serve(listener: TcpListener, shared: &BrokerShared) {
    while !shared.stopping.load(Ordering::Acquire) {
        let (mut stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(error = %error, "accept ssh askpass request failed");
                break;
            }
        };
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        if !peer.ip().is_loopback() {
            continue;
        }
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        if let Err(error) = serve_one(&mut stream, shared) {
            tracing::warn!(error = %error, "serve ssh askpass request failed");
        }
    }
}

fn serve_one(stream: &mut TcpStream, shared: &BrokerShared) -> io::Result<()> {
    let mut token = [0u8; TOKEN_BYTES];
    stream.read_exact(&mut token)?;
    let token = std::str::from_utf8(&token)
        .ok()
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let secret = token
        .and_then(|token| shared.secrets.lock().remove(token))
        .filter(|secret| secret.expires_at > Instant::now());
    let Some(secret) = secret else {
        return stream.write_all(&[0]);
    };
    let length = u32::try_from(secret.bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "password is too long"))?;
    stream.write_all(&[1])?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&secret.bytes)
}

/// AskPass 子进程入口。普通应用启动返回 `None`；AskPass 请求处理后返回退出码。
pub(crate) fn run_helper(confirm: impl FnOnce(&str) -> bool) -> Option<i32> {
    if std::env::var(MARKER_ENV).as_deref() != Ok("1") {
        return None;
    }
    match std::env::var("SSH_ASKPASS_PROMPT").as_deref() {
        Ok("confirm") => {
            let prompt = bounded_prompt(std::env::args().nth(1).unwrap_or_default());
            let answer = if confirm(&prompt) { "yes\n" } else { "no\n" };
            Some(write_answer(answer.as_bytes()))
        }
        Ok("none") => Some(0),
        _ => {
            let result = fetch_secret_from_env().and_then(|mut secret| {
                secret.push(b'\n');
                let result = io::stdout().write_all(&secret);
                secret.zeroize();
                result
            });
            if let Err(error) = result {
                eprintln!("Ramag SSH AskPass 读取一次性密码失败：{error}");
                Some(1)
            } else {
                Some(0)
            }
        }
    }
}

fn write_answer(answer: &[u8]) -> i32 {
    if io::stdout().write_all(answer).is_ok() {
        0
    } else {
        1
    }
}

fn bounded_prompt(mut value: String) -> String {
    let mut end = value.len().min(MAX_PROMPT_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.retain(|character| !character.is_control() || character == '\n');
    value
}

fn fetch_secret_from_env() -> io::Result<Vec<u8>> {
    let endpoint = std::env::var(ENDPOINT_ENV)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "missing endpoint"))?;
    let token = std::env::var(TOKEN_ENV)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "missing token"))?;
    fetch_secret(&endpoint, &token)
}

fn fetch_secret(endpoint: &str, token: &str) -> io::Result<Vec<u8>> {
    if token.len() != TOKEN_BYTES || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid token"));
    }
    let address: SocketAddr = endpoint
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid endpoint"))?;
    if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "endpoint is not loopback",
        ));
    }
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(token.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;

    let mut status = [0u8; 1];
    stream.read_exact(&mut status)?;
    if status[0] != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential expired or already used",
        ));
    }
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_SSH_PASSWORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid credential length",
        ));
    }
    let mut secret = vec![0; length];
    stream.read_exact(&mut secret)?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_secret_is_single_use_and_never_enters_environment() {
        let broker = AskPassBroker::new();
        let mut profile = SshProfile::new("server", "example.com");
        profile.auth_mode = SshAuthMode::Password;
        profile.password = "secret-value".into();

        let environment = broker.environment(&profile).unwrap();
        assert!(
            environment
                .values()
                .all(|value| !value.contains("secret-value"))
        );
        let endpoint = environment.get(ENDPOINT_ENV).unwrap();
        let token = environment.get(TOKEN_ENV).unwrap();
        assert_eq!(fetch_secret(endpoint, token).unwrap(), b"secret-value");
        assert!(fetch_secret(endpoint, token).is_err());
    }

    #[test]
    fn broker_rejects_non_loopback_endpoint_and_bad_token() {
        assert!(fetch_secret("192.0.2.1:22", &"a".repeat(TOKEN_BYTES)).is_err());
        assert!(fetch_secret("127.0.0.1:22", "not-a-token").is_err());
    }

    #[test]
    fn broker_rejects_expired_secret() {
        let broker = AskPassBroker::new();
        let mut profile = SshProfile::new("server", "example.com");
        profile.auth_mode = SshAuthMode::Password;
        profile.password = "secret-value".into();
        let environment = broker.environment(&profile).unwrap();
        let token = environment.get(TOKEN_ENV).unwrap().clone();
        {
            let server = broker.server.lock();
            let shared = &server.as_ref().unwrap().shared;
            shared.secrets.lock().get_mut(&token).unwrap().expires_at = Instant::now();
        }

        assert!(fetch_secret(environment.get(ENDPOINT_ENV).unwrap(), &token).is_err());
    }

    #[test]
    fn host_confirmation_prompt_is_byte_bounded_and_sanitized() {
        let prompt = bounded_prompt(format!("{}\0ignored", "主".repeat(MAX_PROMPT_BYTES)));

        assert!(prompt.len() <= MAX_PROMPT_BYTES);
        assert!(!prompt.contains('\0'));
        assert!(prompt.is_char_boundary(prompt.len()));
    }
}
