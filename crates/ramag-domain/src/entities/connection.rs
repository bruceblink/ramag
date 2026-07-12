//! 连接配置实体

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
