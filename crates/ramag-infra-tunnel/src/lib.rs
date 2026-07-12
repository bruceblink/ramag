//! SSH 隧道管理器：系统 ssh 子进程做本地端口转发（`ssh -N -L`）。
//! 与 infra-git 同哲学——密钥 / agent / known_hosts / `~/.ssh/config` 别名全由系统 ssh
//! 处理，不引入 Rust SSH 实现。按 `ConnectionId` 缓存隧道；参数变更或进程死亡自动重建。
//! driver 只在建连处调 `ensure`，把 DB 目标地址换成 `127.0.0.1:本地端口`

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ramag_domain::entities::{ConnectionConfig, ConnectionId};
use ramag_domain::error::{DomainError, Result};
use tracing::info;

/// 隧道就绪探测总时长（含 ssh 认证握手，密钥 / agent 场景通常 1-3s）
const READY_TIMEOUT: Duration = Duration::from_secs(12);
/// 就绪探测轮询间隔
const PROBE_INTERVAL: Duration = Duration::from_millis(200);

struct Tunnel {
    child: Child,
    local_port: u16,
    /// 参数指纹：target / ssh 端口 / 转发目的地任一变更即重建
    fingerprint: String,
}

fn registry() -> &'static Mutex<HashMap<ConnectionId, Tunnel>> {
    static TUNNELS: OnceLock<Mutex<HashMap<ConnectionId, Tunnel>>> = OnceLock::new();
    TUNNELS.get_or_init(|| Mutex::new(HashMap::new()))
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

/// 确保该连接的 SSH 隧道就绪。未启用隧道返回 `None`；
/// 启用则返回应改连的本地地址 `("127.0.0.1", 本地端口)`
pub fn ensure(config: &ConnectionConfig) -> Result<Option<(String, u16)>> {
    let Some(target) = config
        .ssh_target
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };

    let want = fingerprint(config);
    let mut map = registry()
        .lock()
        .map_err(|_| DomainError::QueryFailed("SSH 隧道注册表锁失效".into()))?;

    // 命中：指纹一致且进程仍活着（try_wait None）
    if let Some(t) = map.get_mut(&config.id) {
        let alive = matches!(t.child.try_wait(), Ok(None));
        if alive && t.fingerprint == want {
            return Ok(Some(("127.0.0.1".into(), t.local_port)));
        }
        // 参数变了或进程已死：拆掉重建
        let _ = t.child.kill();
        let _ = t.child.wait();
        map.remove(&config.id);
        info!(connection_id = %config.id, "ssh tunnel rebuilt (stale or dead)");
    }

    let local_port = pick_free_port()?;
    let forward = format!("127.0.0.1:{local_port}:{}:{}", config.host, config.port);
    let mut cmd = ssh_command();
    cmd.args(["-N", "-L", &forward])
        // GUI 无 tty：禁交互提问（密码认证会挂死，须用密钥 / agent，报错里已提示）
        .args(["-o", "BatchMode=yes"])
        .args(["-o", "ExitOnForwardFailure=yes"])
        .args(["-o", "ServerAliveInterval=15"])
        .args(["-o", "ConnectTimeout=10"]);
    if let Some(p) = config.ssh_port.filter(|p| *p != 22) {
        cmd.arg("-p").arg(p.to_string());
    }
    cmd.arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        DomainError::QueryFailed(format!("启动 ssh 失败（请确认系统已安装 OpenSSH）：{e}"))
    })?;

    // 就绪探测：本地端口可连即成；ssh 提前退出则读 stderr 给出可定位原因
    let start = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", local_port)).is_ok() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let stderr = read_stderr(&mut child);
            return Err(DomainError::QueryFailed(format!(
                "SSH 隧道建立失败（exit {status}）：{}（GUI 无法输入密码，请配置密钥或 ssh-agent）",
                stderr.trim()
            )));
        }
        if start.elapsed() > READY_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DomainError::QueryFailed(
                "SSH 隧道就绪超时：请检查跳板机地址与网络连通性".into(),
            ));
        }
        std::thread::sleep(PROBE_INTERVAL);
    }

    info!(connection_id = %config.id, target, local_port, "ssh tunnel established");
    map.insert(
        config.id.clone(),
        Tunnel {
            child,
            local_port,
            fingerprint: want,
        },
    );
    Ok(Some(("127.0.0.1".into(), local_port)))
}

/// 关闭并移除该连接的隧道（driver evict_pool 时调，编辑配置后下次建连按新参数重建）
pub fn evict(id: &ConnectionId) {
    if let Ok(mut map) = registry().lock()
        && let Some(mut t) = map.remove(id)
    {
        let _ = t.child.kill();
        let _ = t.child.wait();
        info!(connection_id = %id, "ssh tunnel closed");
    }
}

/// 关闭全部隧道（应用退出时调，避免残留孤儿 ssh 进程）
pub fn shutdown_all() {
    if let Ok(mut map) = registry().lock() {
        let n = map.len();
        for (_, mut t) in map.drain() {
            let _ = t.child.kill();
            let _ = t.child.wait();
        }
        if n > 0 {
            info!(count = n, "all ssh tunnels closed");
        }
    }
}

/// 绑 :0 让系统分配空闲端口。释放到 ssh 监听之间存在极小竞态窗口，实践可接受
fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| DomainError::QueryFailed(format!("分配本地端口失败：{e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| DomainError::QueryFailed(format!("读取本地端口失败：{e}")))?
        .port();
    Ok(port)
}

fn read_stderr(child: &mut Child) -> String {
    use std::io::Read;
    let mut out = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut out);
    }
    if out.is_empty() {
        out = "无输出".into();
    }
    out
}

/// Windows 上禁止 ssh.exe 弹控制台闪窗（与 git_cmd 同处理）
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
}
