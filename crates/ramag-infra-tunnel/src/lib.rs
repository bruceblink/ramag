//! 基于系统 OpenSSH 的本地端口转发管理。

use std::collections::HashMap;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ramag_domain::entities::{ConnectionConfig, ConnectionId};
use ramag_domain::error::{DomainError, Result};
use tracing::{info, warn};

/// 隧道就绪探测总时长。
const READY_TIMEOUT: Duration = Duration::from_secs(12);
/// 就绪探测轮询间隔。
const PROBE_INTERVAL: Duration = Duration::from_millis(200);
/// 建连失败时最多回传的 ssh stderr，防异常子进程输出导致无界内存与超长提示。
const MAX_ERROR_OUTPUT_BYTES: usize = 16 * 1024;
/// 自定义 CA 证书文件上限。
pub const MAX_CA_CERT_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_TUNNELS: usize = 64;

/// 在连接前校验自定义 CA 文件。
pub fn validate_ca_certificate_file(path: &str) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        DomainError::InvalidConfig(format!("读取 CA 证书元数据失败（{path}）：{error}"))
    })?;
    if !metadata.is_file() {
        return Err(DomainError::InvalidConfig(format!(
            "CA 证书不是普通文件（{path}）"
        )));
    }
    if metadata.len() == 0 {
        return Err(DomainError::InvalidConfig(format!(
            "CA 证书文件为空（{path}）"
        )));
    }
    if metadata.len() > MAX_CA_CERT_BYTES as u64 {
        return Err(DomainError::InvalidConfig(format!(
            "CA 证书超过 {} MiB 安全上限（{path}）",
            MAX_CA_CERT_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

struct Tunnel {
    child: Child,
    local_port: u16,
    /// 连接参数变化时重建隧道。
    fingerprint: String,
    /// 排空 stderr，防止子进程阻塞。
    stderr_drain: Option<JoinHandle<()>>,
}

impl Tunnel {
    fn stop(&mut self) {
        match self.child.try_wait() {
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    warn!(operation = "ssh_tunnel_stop", stage = "kill", error = %error, "ssh tunnel kill failed");
                }
            }
            Ok(Some(_)) => {}
            Err(error) => {
                warn!(operation = "ssh_tunnel_stop", stage = "status", error = %error, "ssh tunnel status check failed");
                if let Err(kill_error) = self.child.kill() {
                    warn!(operation = "ssh_tunnel_stop", stage = "kill_after_status_error", error = %kill_error, "ssh tunnel kill after status error failed");
                }
            }
        }
        if let Err(error) = self.child.wait() {
            warn!(operation = "ssh_tunnel_stop", stage = "wait", error = %error, "ssh tunnel wait failed");
        }
        if let Some(drain) = self.stderr_drain.take()
            && drain.join().is_err()
        {
            warn!(
                operation = "ssh_tunnel_stop",
                stage = "stderr_drain",
                "ssh stderr drain thread panicked"
            );
        }
    }
}

type TunnelSlot = Arc<Mutex<Option<Tunnel>>>;

fn registry() -> &'static Mutex<HashMap<ConnectionId, TunnelSlot>> {
    static TUNNELS: OnceLock<Mutex<HashMap<ConnectionId, TunnelSlot>>> = OnceLock::new();
    TUNNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn slot_for(id: &ConnectionId) -> Result<TunnelSlot> {
    let mut map = registry()
        .lock()
        .map_err(|_| DomainError::QueryFailed("SSH 隧道注册表锁失效".into()))?;
    if !can_allocate_tunnel_slot(map.len(), map.contains_key(id)) {
        return Err(DomainError::QueryFailed(format!(
            "SSH 隧道已达 {MAX_ACTIVE_TUNNELS} 个并发上限，请先关闭不再使用的连接"
        )));
    }
    Ok(map
        .entry(id.clone())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone())
}

fn can_allocate_tunnel_slot(current: usize, already_exists: bool) -> bool {
    already_exists || current < MAX_ACTIVE_TUNNELS
}

fn fingerprint(config: &ConnectionConfig) -> String {
    format!(
        "{}|{}|{}:{}",
        config.ssh_target.as_deref().unwrap_or_default(),
        config.ssh_port.unwrap_or(22),
        config.host,
        config.port
    )
}

/// 确保 SSH 隧道就绪，并返回本地转发地址。
pub fn ensure(config: &ConnectionConfig) -> Result<Option<(String, u16)>> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    let Some(target) = config
        .ssh_target
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };

    let want = fingerprint(config);
    // 全局锁仅管理索引，握手只锁当前连接。
    let slot = slot_for(&config.id)?;
    let mut current = slot
        .lock()
        .map_err(|_| DomainError::QueryFailed("SSH 隧道连接锁失效".into()))?;

    let reusable = current
        .as_mut()
        .is_some_and(|t| matches!(t.child.try_wait(), Ok(None)) && t.fingerprint == want);
    if reusable && let Some(t) = current.as_ref() {
        return Ok(Some(("127.0.0.1".into(), t.local_port)));
    }
    if let Some(mut stale) = current.take() {
        stale.stop();
        info!(operation = "ssh_tunnel_ensure", stage = "rebuild", connection_id = %config.id, "ssh tunnel rebuilt (stale or dead)");
    }

    let tunnel = match build_tunnel(config, target, want) {
        Ok(tunnel) => tunnel,
        Err(error) => {
            drop(current);
            remove_unused_slot(&config.id, &slot);
            return Err(error);
        }
    };
    let local_port = tunnel.local_port;
    *current = Some(tunnel);
    info!(operation = "ssh_tunnel_ensure", stage = "established", connection_id = %config.id, target, local_port, "ssh tunnel established");
    Ok(Some(("127.0.0.1".into(), local_port)))
}

fn remove_unused_slot(id: &ConnectionId, slot: &TunnelSlot) {
    let Ok(mut map) = registry().lock() else {
        warn!(operation = "ssh_tunnel_cleanup", stage = "registry_lock", connection_id = %id, "ssh tunnel registry lock poisoned during failed setup cleanup");
        return;
    };
    let is_current = map
        .get(id)
        .is_some_and(|registered| Arc::ptr_eq(registered, slot));
    // 额外 Arc 表示仍有并发 ensure/evict，不能移除。
    let is_empty = slot.lock().is_ok_and(|current| current.is_none());
    if is_current && Arc::strong_count(slot) == 2 && is_empty {
        map.remove(id);
    }
}

fn build_tunnel(config: &ConnectionConfig, target: &str, fingerprint: String) -> Result<Tunnel> {
    let local_port = pick_free_port()?;
    let forward = format!("127.0.0.1:{local_port}:{}:{}", config.host, config.port);
    let mut cmd = ssh_command();
    cmd.args(["-N", "-L", &forward])
        // GUI 无 TTY，禁用交互认证以避免挂起。
        .args(["-o", "BatchMode=yes"])
        .args(["-o", "ExitOnForwardFailure=yes"])
        .args(["-o", "ServerAliveInterval=15"])
        .args(["-o", "ConnectTimeout=10"]);
    if let Some(p) = config.ssh_port.filter(|p| *p != 22) {
        cmd.arg("-p").arg(p.to_string());
    }
    append_destination(&mut cmd, target);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        DomainError::QueryFailed(format!("启动 ssh 失败（请确认系统已安装 OpenSSH）：{e}"))
    })?;

    // 本地端口可连即就绪；提前退出时返回 stderr。
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", local_port)).is_ok() {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = read_stderr(&mut child);
                return Err(DomainError::QueryFailed(format!(
                    "SSH 隧道建立失败（exit {status}）：{}（GUI 无法输入密码，请配置密钥或 ssh-agent）",
                    stderr.trim()
                )));
            }
            Ok(None) => {}
            Err(error) => {
                terminate_child(&mut child);
                return Err(DomainError::QueryFailed(format!(
                    "检查 SSH 隧道进程失败：{error}"
                )));
            }
        }
        if start.elapsed() > READY_TIMEOUT {
            terminate_child(&mut child);
            return Err(DomainError::QueryFailed(
                "SSH 隧道就绪超时：请检查跳板机地址与网络连通性".into(),
            ));
        }
        std::thread::sleep(PROBE_INTERVAL);
    }

    let stderr_drain = match start_stderr_drain(&mut child, local_port) {
        Ok(drain) => drain,
        Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        }
    };
    Ok(Tunnel {
        child,
        local_port,
        fingerprint,
        stderr_drain: Some(stderr_drain),
    })
}

fn append_destination(command: &mut Command, target: &str) {
    // 防止目标被 OpenSSH 解析为选项。
    command.arg("--").arg(target);
}

/// 关闭并移除连接的隧道。
pub fn evict(id: &ConnectionId) {
    let slot = match registry().lock() {
        Ok(mut map) => map.remove(id),
        Err(_) => {
            warn!(operation = "ssh_tunnel_evict", stage = "registry_lock", connection_id = %id, "ssh tunnel registry lock poisoned during evict");
            return;
        }
    };
    if let Some(slot) = slot {
        match slot.lock() {
            Ok(mut current) => {
                if let Some(mut tunnel) = current.take() {
                    tunnel.stop();
                    info!(operation = "ssh_tunnel_evict", connection_id = %id, "ssh tunnel closed");
                }
            }
            Err(_) => {
                warn!(operation = "ssh_tunnel_evict", stage = "slot_lock", connection_id = %id, "ssh tunnel slot lock poisoned during evict")
            }
        }
    }
}

/// 关闭全部隧道。
pub fn shutdown_all() {
    let slots: Vec<TunnelSlot> = match registry().lock() {
        Ok(mut map) => map.drain().map(|(_, slot)| slot).collect(),
        Err(_) => {
            warn!(
                operation = "ssh_tunnel_shutdown",
                stage = "registry_lock",
                "ssh tunnel registry lock poisoned during shutdown"
            );
            return;
        }
    };
    let mut closed = 0usize;
    for slot in slots {
        match slot.lock() {
            Ok(mut current) => {
                if let Some(mut tunnel) = current.take() {
                    tunnel.stop();
                    closed += 1;
                }
            }
            Err(_) => warn!(
                operation = "ssh_tunnel_shutdown",
                stage = "slot_lock",
                "ssh tunnel slot lock poisoned during shutdown"
            ),
        }
    }
    if closed > 0 {
        info!(
            operation = "ssh_tunnel_shutdown",
            count = closed,
            "all ssh tunnels closed"
        );
    }
}

/// 让系统分配临时端口；释放到监听间存在极小竞态窗口。
fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| DomainError::QueryFailed(format!("分配本地端口失败：{e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| DomainError::QueryFailed(format!("读取本地端口失败：{e}")))?
        .port();
    Ok(port)
}

fn start_stderr_drain(child: &mut Child, local_port: u16) -> Result<JoinHandle<()>> {
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| DomainError::QueryFailed("无法读取 SSH 隧道错误输出".into()))?;
    std::thread::Builder::new()
        .name(format!("ramag-ssh-stderr-{local_port}"))
        .spawn(move || {
            if let Err(error) = std::io::copy(&mut stderr, &mut std::io::sink()) {
                warn!(operation = "ssh_tunnel_stderr_drain", error = %error, local_port, "ssh stderr drain failed");
            }
        })
        .map_err(|error| DomainError::QueryFailed(format!("启动 SSH 输出排空线程失败：{error}")))
}

fn terminate_child(child: &mut Child) {
    if let Err(error) = child.kill() {
        warn!(operation = "ssh_tunnel_setup_cleanup", stage = "kill", error = %error, "ssh tunnel kill failed during setup");
    }
    if let Err(error) = child.wait() {
        warn!(operation = "ssh_tunnel_setup_cleanup", stage = "wait", error = %error, "ssh tunnel wait failed during setup");
    }
}

fn read_stderr(child: &mut Child) -> String {
    let mut bytes = Vec::new();
    if let Some(mut s) = child.stderr.take()
        && let Err(error) = s
            .by_ref()
            .take((MAX_ERROR_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
    {
        return format!("读取 ssh 错误输出失败：{error}");
    }
    if bytes.is_empty() {
        return "无输出".into();
    }
    let truncated = bytes.len() > MAX_ERROR_OUTPUT_BYTES;
    bytes.truncate(MAX_ERROR_OUTPUT_BYTES);
    let mut out = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        out.push_str("\n…输出已截断");
    }
    out
}

/// Windows 上禁止 ssh.exe 弹出控制台窗口。
fn ssh_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("ssh");
        c.creation_flags(CREATE_NO_WINDOW);
        c
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("ssh")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_target_means_no_tunnel() {
        let cfg = ConnectionConfig::new_mysql("t", "db.internal", 3306, "root");
        assert!(matches!(ensure(&cfg), Ok(None)));
    }

    #[test]
    fn invalid_config_is_rejected_without_starting_ssh() {
        let mut cfg = ConnectionConfig::new_mysql("t", "db.internal", 3306, "root");
        cfg.port = 0;
        assert!(matches!(ensure(&cfg), Err(DomainError::InvalidConfig(_))));
    }

    #[test]
    fn fingerprint_covers_target_port_and_destination() {
        let mut cfg = ConnectionConfig::new_mysql("t", "db.internal", 3306, "root");
        cfg.ssh_target = Some("ops@bastion".into());
        let a = fingerprint(&cfg);
        cfg.ssh_port = Some(2222);
        let b = fingerprint(&cfg);
        cfg.host = "db2.internal".into();
        let c = fingerprint(&cfg);
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn free_port_is_nonzero() {
        assert!(pick_free_port().unwrap_or(0) > 0);
    }

    #[test]
    fn ssh_destination_cannot_be_interpreted_as_an_option() {
        let mut command = Command::new("ssh");
        append_destination(&mut command, "-oProxyCommand=unsafe");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                std::ffi::OsStr::new("--"),
                std::ffi::OsStr::new("-oProxyCommand=unsafe")
            ]
        );
    }

    #[test]
    fn ca_certificate_file_has_an_explicit_size_boundary()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ramag-ca-boundary-{}-{unique}.pem",
            std::process::id()
        ));

        std::fs::write(&path, vec![b'x'; MAX_CA_CERT_BYTES])?;
        assert!(validate_ca_certificate_file(&path.to_string_lossy()).is_ok());
        std::fs::write(&path, vec![b'x'; MAX_CA_CERT_BYTES + 1])?;
        assert!(validate_ca_certificate_file(&path.to_string_lossy()).is_err());
        std::fs::write(&path, [])?;
        assert!(validate_ca_certificate_file(&path.to_string_lossy()).is_err());
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn slots_serialize_only_the_same_connection()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();
        let first = slot_for(&first_id)?;
        let first_again = slot_for(&first_id)?;
        let second = slot_for(&second_id)?;

        assert!(Arc::ptr_eq(&first, &first_again));
        assert!(!Arc::ptr_eq(&first, &second));

        evict(&first_id);
        evict(&second_id);
        Ok(())
    }

    #[test]
    fn failed_setup_cleanup_removes_an_unshared_empty_slot()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let id = ConnectionId::new();
        let slot = slot_for(&id)?;
        assert!(registry().lock()?.contains_key(&id));

        remove_unused_slot(&id, &slot);

        assert!(!registry().lock()?.contains_key(&id));
        Ok(())
    }

    #[test]
    fn active_tunnel_limit_has_an_exact_boundary() {
        assert!(can_allocate_tunnel_slot(MAX_ACTIVE_TUNNELS - 1, false));
        assert!(!can_allocate_tunnel_slot(MAX_ACTIVE_TUNNELS, false));
        assert!(can_allocate_tunnel_slot(MAX_ACTIVE_TUNNELS, true));
    }
}
