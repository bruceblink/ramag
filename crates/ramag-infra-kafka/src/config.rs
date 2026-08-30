//! 将领域配置转换为固定 allowlist 内的 librdkafka 属性。

use std::time::Duration;

use ramag_domain::entities::{KafkaClusterConfig, TlsVerify};
use ramag_domain::error::{DomainError, Result};
use rdkafka::ClientConfig;

/// 仅写入代码明确支持的客户端属性，拒绝任意 Map 透传造成的配置注入。
pub(crate) fn build_client_config(
    config: &KafkaClusterConfig,
    request_timeout: Duration,
) -> Result<ClientConfig> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    let timeout_ms = request_timeout_millis(request_timeout)?;
    let mut client = ClientConfig::new();
    client
        .set("bootstrap.servers", config.bootstrap_servers.join(","))
        .set(
            "client.id",
            config.client_id.as_deref().unwrap_or("ramag-kafka"),
        )
        .set(
            "security.protocol",
            config.security_protocol.as_client_property(),
        )
        .set("request.timeout.ms", &timeout_ms)
        .set("socket.timeout.ms", &timeout_ms)
        .set("metadata.request.timeout.ms", &timeout_ms)
        .set("enable.auto.commit", "false")
        .set("enable.auto.offset.store", "false")
        .set("allow.auto.create.topics", "false");

    if let Some(mechanism) = config.sasl_mechanism {
        client.set("sasl.mechanisms", mechanism.as_client_property());
    }
    if let Some(username) = config.sasl_username.as_deref() {
        client.set("sasl.username", username);
    }
    if let Some(password) = config.sasl_password.as_deref() {
        client.set("sasl.password", password);
    }
    if let Some(path) = config.tls.ca_cert_path.as_deref() {
        client.set("ssl.ca.location", path);
    }
    if let Some(path) = config.tls.client_cert_path.as_deref() {
        client.set("ssl.certificate.location", path);
    }
    if let Some(path) = config.tls.client_key_path.as_deref() {
        client.set("ssl.key.location", path);
    }
    match config.tls.verify {
        TlsVerify::None => {
            client.set("enable.ssl.certificate.verification", "false");
        }
        TlsVerify::Ca => {
            client.set("ssl.endpoint.identification.algorithm", "none");
        }
        TlsVerify::Full => {}
    }
    Ok(client)
}

/// 将 Rust 超时转换成 librdkafka 的毫秒属性，并拒绝转换后为零的值。
pub(crate) fn request_timeout_millis(request_timeout: Duration) -> Result<String> {
    let millis = request_timeout.as_millis();
    if millis == 0 {
        return Err(DomainError::InvalidConfig(
            "Kafka 请求超时必须至少为 1 毫秒".into(),
        ));
    }
    Ok(millis.min(u32::MAX as u128).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{KafkaSaslMechanism, KafkaSecurityProtocol};

    #[test]
    fn client_config_maps_supported_security_properties() {
        let mut config = KafkaClusterConfig::new("secure", vec!["broker:9093".into()]);
        config.security_protocol = KafkaSecurityProtocol::SaslSsl;
        config.sasl_mechanism = Some(KafkaSaslMechanism::ScramSha256);
        config.sasl_username = Some("user".into());
        config.sasl_password = Some("password".into());
        config.tls.ca_cert_path = Some("C:\\certs\\ca.pem".into());
        config.tls.verify = TlsVerify::Ca;

        let result = build_client_config(&config, Duration::from_secs(3));
        assert!(result.is_ok());
        let client = match result {
            Ok(client) => client,
            Err(_) => return,
        };

        assert_eq!(client.get("bootstrap.servers"), Some("broker:9093"));
        assert_eq!(client.get("security.protocol"), Some("SASL_SSL"));
        assert_eq!(client.get("sasl.mechanisms"), Some("SCRAM-SHA-256"));
        assert_eq!(client.get("enable.auto.commit"), Some("false"));
        assert_eq!(
            client.get("ssl.endpoint.identification.algorithm"),
            Some("none")
        );
        assert_ne!(client.config_map().get("sasl.password"), Some(&"password"));
        assert_eq!(client.get("not.allowed"), None);
    }

    #[test]
    fn client_config_rejects_zero_timeout_and_invalid_domain_config() {
        let config = KafkaClusterConfig::new("valid", vec!["broker:9092".into()]);
        assert!(build_client_config(&config, Duration::ZERO).is_err());
        assert!(build_client_config(&config, Duration::from_nanos(1)).is_err());

        let invalid = KafkaClusterConfig::new("invalid", vec!["broker".into()]);
        assert!(build_client_config(&invalid, Duration::from_secs(1)).is_err());
    }
}
