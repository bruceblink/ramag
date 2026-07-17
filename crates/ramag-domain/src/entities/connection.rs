//! 连接配置实体

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 连接名称用于列表渲染与日志字段，限制异常输入造成的内存与布局开销。
pub const MAX_CONNECTION_NAME_BYTES: usize = 256;
/// TCP 主机名 / IP 的资源边界；保留足够空间兼容较长内部域名。
pub const MAX_CONNECTION_HOST_BYTES: usize = 1024;
/// 用户名、数据库名与 MongoDB authSource 的统一资源边界。
pub const MAX_CONNECTION_IDENTIFIER_BYTES: usize = 4 * 1024;
/// 密码只在内存中保留明文，但加密与 hex 编码会放大体积，故单独限制。
pub const MAX_CONNECTION_PASSWORD_BYTES: usize = 64 * 1024;
/// 备注允许多行，但不应让单条连接记录无界增长。
pub const MAX_CONNECTION_REMARK_BYTES: usize = 16 * 1024;
/// Windows 长路径与 Unix 路径均留有余量，同时约束 CA 路径复制成本。
pub const MAX_CONNECTION_PATH_BYTES: usize = 32 * 1024;
/// SSH 目标仅需容纳 user@host 或 config 别名。
pub const MAX_CONNECTION_SSH_TARGET_BYTES: usize = 4 * 1024;

/// 连接唯一标识
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionId(pub Uuid);

impl ConnectionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 数据库类型。Hash 派生用于 `ConnectionService` 的 `HashMap<DriverKind, Arc<dyn Driver>>` dispatch
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriverKind {
    Mysql,
    /// 与 Mysql 共用 SqlBackend 抽象层
    Postgres,
    /// KV 形态，走 KvDriver 而非 Driver
    Redis,
    /// 文档形态，走 DocDriver 而非 Driver / KvDriver
    Mongodb,
}

impl DriverKind {
    /// 按方言加引号包裹标识符。MySQL 反引号、PG 双引号、Redis / MongoDB 原样
    pub fn quote_identifier(&self, ident: &str) -> String {
        match self {
            DriverKind::Mysql => format!("`{}`", ident.replace('`', "``")),
            DriverKind::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
            DriverKind::Redis | DriverKind::Mongodb => ident.to_string(),
        }
    }
}

/// TLS 身份验证等级。「加密」不等于「确认连接的是正确服务器」，故显式分档
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TlsVerify {
    /// 仅加密，不验证服务器证书（自签且无 CA 文件时的最后选择，防窃听不防冒充）
    None,
    /// 验证 CA 证书链，不校验主机名（证书名与连接地址不符 / 经 SSH 隧道时用）
    Ca,
    /// 验证证书链 + 主机名（推荐，完整确认服务器身份）
    #[default]
    Full,
}

/// 连接配置。密码运行时明文，落盘前由 storage 层 AES-GCM 加密
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: ConnectionId,
    pub name: String,
    pub driver: DriverKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: Option<String>,
    /// MongoDB 认证库（authSource）：用户凭证所在库，留空 = admin；其它 driver 不使用
    #[serde(default)]
    pub auth_source: Option<String>,
    pub remark: Option<String>,
    /// 生产模式：开启后由 driver 层拦截一切写 / 改 / 删操作（只读保护）
    #[serde(default)]
    pub production: bool,
    /// 启用 TLS 加密传输（默认关，与历史行为一致；各 driver 底层均走 rustls）
    #[serde(default)]
    pub tls: bool,
    /// TLS 身份验证等级（仅 tls=true 时生效；默认 Full = 验证书链 + 主机名）
    #[serde(default)]
    pub tls_verify: TlsVerify,
    /// 自定义 CA 证书路径（PEM）。仅 tls=true 时生效：留空用系统 / webpki 根证书，
    /// 填写则以该 CA 严格校验服务端证书链（自签证书场景）
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    /// SSH 跳板目标（`user@host` / `host` / `~/.ssh/config` 别名；空 = 不走隧道）。
    /// 认证由系统 ssh 处理（密钥 / agent），经隧道时 DB 实际连 127.0.0.1:本地转发端口
    #[serde(default)]
    pub ssh_target: Option<String>,
    /// SSH 跳板端口（None = 22 或 ~/.ssh/config 决定）
    #[serde(default)]
    pub ssh_port: Option<u16>,
}

impl ConnectionConfig {
    /// 校验所有持久化与建连入口共享的资源 / 安全边界。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("连接名称不能为空".to_string());
        }
        validate_single_line("连接名称", &self.name, MAX_CONNECTION_NAME_BYTES)?;

        if self.host.trim().is_empty() {
            return Err("主机地址不能为空".to_string());
        }
        validate_single_line("主机地址", &self.host, MAX_CONNECTION_HOST_BYTES)?;

        if self.port == 0 {
            return Err("端口必须是 1 - 65535".to_string());
        }
        validate_protocol_text("用户名", &self.username, MAX_CONNECTION_IDENTIFIER_BYTES)?;
        validate_protocol_text("密码", &self.password, MAX_CONNECTION_PASSWORD_BYTES)?;
        validate_optional_protocol_text(
            "数据库名",
            self.database.as_deref(),
            MAX_CONNECTION_IDENTIFIER_BYTES,
        )?;
        validate_optional_protocol_text(
            "认证库",
            self.auth_source.as_deref(),
            MAX_CONNECTION_IDENTIFIER_BYTES,
        )?;
        validate_optional_protocol_text(
            "备注",
            self.remark.as_deref(),
            MAX_CONNECTION_REMARK_BYTES,
        )?;
        validate_optional_single_line(
            "CA 证书路径",
            self.ca_cert_path.as_deref(),
            MAX_CONNECTION_PATH_BYTES,
        )?;
        validate_optional_single_line(
            "SSH 跳板目标",
            self.ssh_target.as_deref(),
            MAX_CONNECTION_SSH_TARGET_BYTES,
        )?;
        if self.ssh_port == Some(0) {
            return Err("SSH 端口必须是 1 - 65535".to_string());
        }
        Ok(())
    }

    pub fn new_mysql(
        name: impl Into<String>,
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
    ) -> Self {
        Self {
            id: ConnectionId::new(),
            name: name.into(),
            driver: DriverKind::Mysql,
            host: host.into(),
            port,
            username: user.into(),
            password: String::new(),
            database: None,
            auth_source: None,
            remark: None,
            production: false,
            tls: false,
            tls_verify: TlsVerify::default(),
            ca_cert_path: None,
            ssh_target: None,
            ssh_port: None,
        }
    }

    /// 构造 Redis 连接（username 留空走老版 AUTH，6.0+ ACL 时填用户名）
    pub fn new_redis(name: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            id: ConnectionId::new(),
            name: name.into(),
            driver: DriverKind::Redis,
            host: host.into(),
            port,
            username: String::new(),
            password: String::new(),
            database: None,
            auth_source: None,
            remark: None,
            production: false,
            tls: false,
            tls_verify: TlsVerify::default(),
            ca_cert_path: None,
            ssh_target: None,
            ssh_port: None,
        }
    }

    /// 构造 MongoDB 连接。database 可选，留空表示默认 `admin`
    pub fn new_mongodb(name: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            id: ConnectionId::new(),
            name: name.into(),
            driver: DriverKind::Mongodb,
            host: host.into(),
            port,
            username: String::new(),
            password: String::new(),
            database: None,
            auth_source: None,
            remark: None,
            production: false,
            tls: false,
            tls_verify: TlsVerify::default(),
            ca_cert_path: None,
            ssh_target: None,
            ssh_port: None,
        }
    }
}

fn validate_optional_single_line(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_single_line(label, value, max_bytes)?;
    }
    Ok(())
}

fn validate_single_line(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    validate_protocol_text(label, value, max_bytes)?;
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

fn validate_optional_protocol_text(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_protocol_text(label, value, max_bytes)?;
    }
    Ok(())
}

fn validate_protocol_text(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!(
            "{label}过长：{} bytes，最多 {max_bytes} bytes",
            value.len()
        ));
    }
    if value.contains('\0') {
        return Err(format!("{label}不能包含 NUL 字符"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ConnectionConfig {
        ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root")
    }

    fn assert_oversized_rejected(limit: usize, set: impl FnOnce(&mut ConnectionConfig, String)) {
        let mut config = valid_config();
        set(&mut config, "x".repeat(limit + 1));
        assert!(config.validate().is_err());
    }

    #[test]
    fn connection_validation_accepts_exact_resource_boundaries() {
        let mut config = valid_config();
        config.name = "n".repeat(MAX_CONNECTION_NAME_BYTES);
        config.host = "h".repeat(MAX_CONNECTION_HOST_BYTES);
        config.username = "u".repeat(MAX_CONNECTION_IDENTIFIER_BYTES);
        config.password = "p".repeat(MAX_CONNECTION_PASSWORD_BYTES);
        config.database = Some("d".repeat(MAX_CONNECTION_IDENTIFIER_BYTES));
        config.auth_source = Some("a".repeat(MAX_CONNECTION_IDENTIFIER_BYTES));
        config.remark = Some("r".repeat(MAX_CONNECTION_REMARK_BYTES));
        config.ca_cert_path = Some("c".repeat(MAX_CONNECTION_PATH_BYTES));
        config.ssh_target = Some("s".repeat(MAX_CONNECTION_SSH_TARGET_BYTES));
        config.ssh_port = Some(u16::MAX);

        assert!(config.validate().is_ok());
    }

    #[test]
    fn connection_validation_rejects_oversized_fields() {
        assert_oversized_rejected(MAX_CONNECTION_NAME_BYTES, |config, value| {
            config.name = value;
        });
        assert_oversized_rejected(MAX_CONNECTION_HOST_BYTES, |config, value| {
            config.host = value;
        });
        assert_oversized_rejected(MAX_CONNECTION_IDENTIFIER_BYTES, |config, value| {
            config.username = value;
        });
        assert_oversized_rejected(MAX_CONNECTION_PASSWORD_BYTES, |config, value| {
            config.password = value;
        });
        assert_oversized_rejected(MAX_CONNECTION_IDENTIFIER_BYTES, |config, value| {
            config.database = Some(value);
        });
        assert_oversized_rejected(MAX_CONNECTION_IDENTIFIER_BYTES, |config, value| {
            config.auth_source = Some(value);
        });
        assert_oversized_rejected(MAX_CONNECTION_REMARK_BYTES, |config, value| {
            config.remark = Some(value);
        });
        assert_oversized_rejected(MAX_CONNECTION_PATH_BYTES, |config, value| {
            config.ca_cert_path = Some(value);
        });
        assert_oversized_rejected(MAX_CONNECTION_SSH_TARGET_BYTES, |config, value| {
            config.ssh_target = Some(value);
        });
    }

    #[test]
    fn connection_validation_rejects_invalid_ports_and_control_characters() {
        let mut config = valid_config();
        config.port = 0;
        assert!(config.validate().is_err());

        config = valid_config();
        config.ssh_port = Some(0);
        assert!(config.validate().is_err());

        config = valid_config();
        config.name = "bad\nname".to_string();
        assert!(config.validate().is_err());

        config = valid_config();
        config.host = "bad\0host".to_string();
        assert!(config.validate().is_err());

        config = valid_config();
        config.password = "bad\0password".to_string();
        assert!(config.validate().is_err());

        config = valid_config();
        config.remark = Some("multi\nline".to_string());
        assert!(config.validate().is_ok());
    }
}
