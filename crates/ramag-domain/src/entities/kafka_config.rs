use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::connection::TlsVerify;
use super::kafka_validation::{
    validate_optional_path, validate_optional_protocol_text, validate_optional_single_line,
    validate_required_text,
};
use super::{
    MAX_KAFKA_BOOTSTRAP_SERVERS, MAX_KAFKA_BOOTSTRAP_SERVERS_BYTES, MAX_KAFKA_CLIENT_ID_BYTES,
    MAX_KAFKA_CLUSTER_NAME_BYTES, MAX_KAFKA_REMARK_BYTES, MAX_KAFKA_SASL_PASSWORD_BYTES,
    MAX_KAFKA_SASL_USERNAME_BYTES, MAX_KAFKA_TLS_PATH_BYTES, validate_kafka_bootstrap_server,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KafkaClusterId(pub Uuid);

impl KafkaClusterId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for KafkaClusterId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for KafkaClusterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaSecurityProtocol {
    #[default]
    Plaintext,
    Ssl,
    SaslPlaintext,
    SaslSsl,
}

impl KafkaSecurityProtocol {
    pub const fn uses_tls(self) -> bool {
        matches!(self, Self::Ssl | Self::SaslSsl)
    }

    pub const fn uses_sasl(self) -> bool {
        matches!(self, Self::SaslPlaintext | Self::SaslSsl)
    }

    pub const fn as_client_property(self) -> &'static str {
        match self {
            Self::Plaintext => "PLAINTEXT",
            Self::Ssl => "SSL",
            Self::SaslPlaintext => "SASL_PLAINTEXT",
            Self::SaslSsl => "SASL_SSL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaSaslMechanism {
    Gssapi,
    Plain,
    ScramSha256,
    ScramSha512,
    OAuthBearer,
}

impl KafkaSaslMechanism {
    pub const fn as_client_property(self) -> &'static str {
        match self {
            Self::Gssapi => "GSSAPI",
            Self::Plain => "PLAIN",
            Self::ScramSha256 => "SCRAM-SHA-256",
            Self::ScramSha512 => "SCRAM-SHA-512",
            Self::OAuthBearer => "OAUTHBEARER",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTlsConfig {
    #[serde(default)]
    pub verify: TlsVerify,
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    #[serde(default)]
    pub client_cert_path: Option<String>,
    #[serde(default)]
    pub client_key_path: Option<String>,
}

impl KafkaTlsConfig {
    /// 校验证书路径的长度和字符，避免把控制字符或超长路径传给客户端库。
    pub fn validate(&self) -> Result<(), String> {
        validate_optional_path(
            "CA 证书路径",
            self.ca_cert_path.as_deref(),
            MAX_KAFKA_TLS_PATH_BYTES,
        )?;
        validate_optional_path(
            "客户端证书路径",
            self.client_cert_path.as_deref(),
            MAX_KAFKA_TLS_PATH_BYTES,
        )?;
        validate_optional_path(
            "客户端密钥路径",
            self.client_key_path.as_deref(),
            MAX_KAFKA_TLS_PATH_BYTES,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaReadOnlyState {
    #[default]
    ReadOnly,
    ReadWrite,
}

impl KafkaReadOnlyState {
    pub const fn allows_admin(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaClusterConfig {
    pub id: KafkaClusterId,
    pub name: String,
    pub bootstrap_servers: Vec<String>,
    #[serde(default)]
    pub security_protocol: KafkaSecurityProtocol,
    #[serde(default)]
    pub sasl_mechanism: Option<KafkaSaslMechanism>,
    #[serde(default)]
    pub sasl_username: Option<String>,
    #[serde(default)]
    pub sasl_password: Option<String>,
    #[serde(default)]
    pub tls: KafkaTlsConfig,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub remark: Option<String>,
    #[serde(default)]
    pub read_only: KafkaReadOnlyState,
}

impl KafkaClusterConfig {
    /// 创建默认只读集群配置；调用方仍须在保存或建连前调用 `validate`。
    pub fn new(name: impl Into<String>, bootstrap_servers: Vec<String>) -> Self {
        Self {
            id: KafkaClusterId::new(),
            name: name.into(),
            bootstrap_servers,
            security_protocol: KafkaSecurityProtocol::default(),
            sasl_mechanism: None,
            sasl_username: None,
            sasl_password: None,
            tls: KafkaTlsConfig::default(),
            client_id: None,
            remark: None,
            read_only: KafkaReadOnlyState::default(),
        }
    }

    /// 校验持久化配置、客户端属性和资源消耗边界。
    pub fn validate(&self) -> Result<(), String> {
        validate_required_text("集群名称", &self.name, MAX_KAFKA_CLUSTER_NAME_BYTES)?;
        if self.bootstrap_servers.is_empty() {
            return Err("Bootstrap Server 不能为空".into());
        }
        if self.bootstrap_servers.len() > MAX_KAFKA_BOOTSTRAP_SERVERS {
            return Err(format!(
                "Bootstrap Server 数量超过 {MAX_KAFKA_BOOTSTRAP_SERVERS} 个上限"
            ));
        }

        let mut total_bytes = 0usize;
        let mut seen = HashSet::with_capacity(self.bootstrap_servers.len());
        for server in &self.bootstrap_servers {
            validate_kafka_bootstrap_server(server)?;
            total_bytes = total_bytes
                .checked_add(server.len())
                .ok_or_else(|| "Bootstrap Server 总长度溢出".to_string())?;
            if !seen.insert(server.as_str()) {
                return Err(format!("Bootstrap Server 重复：{server}"));
            }
        }
        if total_bytes > MAX_KAFKA_BOOTSTRAP_SERVERS_BYTES {
            return Err(format!(
                "Bootstrap Server 总长度超过 {MAX_KAFKA_BOOTSTRAP_SERVERS_BYTES} bytes"
            ));
        }

        validate_optional_single_line(
            "Client ID",
            self.client_id.as_deref(),
            MAX_KAFKA_CLIENT_ID_BYTES,
        )?;
        validate_optional_single_line("备注", self.remark.as_deref(), MAX_KAFKA_REMARK_BYTES)?;
        self.tls.validate()?;

        if self.security_protocol.uses_sasl() {
            if self.sasl_mechanism.is_none() {
                return Err("SASL 协议必须选择 SASL 机制".into());
            }
            validate_optional_protocol_text(
                "SASL 用户名",
                self.sasl_username.as_deref(),
                MAX_KAFKA_SASL_USERNAME_BYTES,
            )?;
            validate_optional_protocol_text(
                "SASL 密码",
                self.sasl_password.as_deref(),
                MAX_KAFKA_SASL_PASSWORD_BYTES,
            )?;
        } else if self.sasl_mechanism.is_some()
            || self.sasl_username.is_some()
            || self.sasl_password.is_some()
        {
            return Err("非 SASL 协议不能设置 SASL 认证参数".into());
        }

        if !self.security_protocol.uses_tls()
            && (self.tls.ca_cert_path.is_some()
                || self.tls.client_cert_path.is_some()
                || self.tls.client_key_path.is_some())
        {
            return Err("非 TLS 协议不能设置证书路径".into());
        }

        Ok(())
    }

    pub const fn uses_tls(&self) -> bool {
        self.security_protocol.uses_tls()
    }

    pub const fn uses_sasl(&self) -> bool {
        self.security_protocol.uses_sasl()
    }
}
