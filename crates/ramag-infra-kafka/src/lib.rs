//! Kafka 基础设施层：使用 `rdkafka` 创建隔离的客户端，并将错误映射为安全领域错误。

#[cfg(feature = "cmake-build")]
mod config;
#[cfg(feature = "cmake-build")]
pub mod errors;

use std::time::Duration;

use ramag_domain::entities::KafkaClusterConfig;
use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory, Result};
use ramag_domain::traits::KafkaDriver;
#[cfg(feature = "cmake-build")]
use rdkafka::admin::AdminClient;
#[cfg(feature = "cmake-build")]
use rdkafka::client::DefaultClientContext;
use tracing::{debug, info};

pub const DEFAULT_KAFKA_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct RdkafkaDriver {
    request_timeout: Duration,
}

impl RdkafkaDriver {
    /// 创建使用固定默认请求预算的 Kafka 驱动。
    pub fn new() -> Self {
        Self {
            request_timeout: DEFAULT_KAFKA_REQUEST_TIMEOUT,
        }
    }

    /// 创建可指定请求预算的驱动，供集成测试和调用方控制连接等待时间。
    pub fn with_request_timeout(request_timeout: Duration) -> Result<Self> {
        validate_request_timeout(request_timeout)?;
        Ok(Self { request_timeout })
    }

    /// 在进入 native 客户端前检查构建能力，避免把不支持的安全配置交给底层库。
    fn ensure_build_features(config: &KafkaClusterConfig) -> Result<()> {
        if config.uses_sasl() && !cfg!(feature = "kafka-sasl") {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Unsupported,
                "创建连接客户端",
                "当前构建未启用 Kafka SASL 支持",
            )));
        }
        if config.uses_tls() && !cfg!(feature = "kafka-tls") {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Tls,
                "创建连接客户端",
                "当前构建未启用 Kafka TLS 支持",
            )));
        }
        Ok(())
    }

    #[cfg(not(feature = "cmake-build"))]
    /// 默认构建不拉入 librdkafka；启用 `cmake-build` 后才提供真实连接能力。
    fn test_connection_blocking(&self, config: &KafkaClusterConfig) -> Result<()> {
        let _ = self.request_timeout;
        Self::ensure_build_features(config)?;
        Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Unsupported,
            "创建连接客户端",
            "当前构建未启用 Kafka native 客户端；请启用 cmake-build feature",
        )))
    }

    #[cfg(feature = "cmake-build")]
    /// 在阻塞线程中创建 Admin Client 并拉取集群元数据，以验证连接配置和网络可达性。
    fn test_connection_blocking(&self, config: &KafkaClusterConfig) -> Result<()> {
        Self::ensure_build_features(config)?;
        let client_config = config::build_client_config(config, self.request_timeout)?;
        let admin: AdminClient<DefaultClientContext> = client_config
            .create()
            .map_err(|error| errors::map_kafka_error(error, "创建连接客户端"))?;
        admin
            .inner()
            .fetch_metadata(None, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "测试 Kafka 连接"))?;
        Ok(())
    }
}

/// 校验对外暴露的请求时间预算，防止亚毫秒值转换后变成无效的零超时。
fn validate_request_timeout(request_timeout: Duration) -> Result<()> {
    if request_timeout.as_millis() == 0 {
        return Err(DomainError::InvalidConfig(
            "Kafka 请求超时必须至少为 1 毫秒".into(),
        ));
    }
    Ok(())
}

impl Default for RdkafkaDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl KafkaDriver for RdkafkaDriver {
    async fn test_connection(&self, config: &KafkaClusterConfig) -> Result<()> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        debug!(
            operation = "kafka_test_connection",
            "starting Kafka connection test"
        );
        smol::unblock(move || driver.test_connection_blocking(&config)).await?;
        info!(
            operation = "kafka_test_connection",
            "Kafka connection test passed"
        );
        Ok(())
    }

    async fn cluster_metadata(
        &self,
        _config: &KafkaClusterConfig,
    ) -> Result<ramag_domain::entities::KafkaClusterMetadata> {
        Err(DomainError::NotImplemented("cluster_metadata".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::error::KafkaErrorCategory;

    #[test]
    fn request_timeout_must_be_positive() {
        assert!(RdkafkaDriver::with_request_timeout(Duration::ZERO).is_err());
        assert!(RdkafkaDriver::with_request_timeout(Duration::from_nanos(1)).is_err());
        assert!(RdkafkaDriver::with_request_timeout(Duration::from_millis(1)).is_ok());
    }

    #[test]
    fn async_connection_test_rejects_invalid_config_before_network() {
        let config = KafkaClusterConfig::new("invalid", vec!["localhost".into()]);
        let driver = RdkafkaDriver::new();
        let result = smol::block_on(driver.test_connection(&config));
        assert!(result.is_err());
        let error = match result {
            Ok(()) => return,
            Err(error) => error,
        };
        assert!(matches!(error, DomainError::InvalidConfig(_)));
    }

    #[cfg(not(feature = "kafka-tls"))]
    #[test]
    fn tls_connection_requires_build_feature() {
        let mut config = KafkaClusterConfig::new("secure", vec!["broker:9093".into()]);
        config.security_protocol = ramag_domain::entities::KafkaSecurityProtocol::Ssl;
        let result = RdkafkaDriver::new().test_connection_blocking(&config);
        assert!(result.is_err());
        let error = match result {
            Ok(()) => return,
            Err(error) => error,
        };
        assert!(matches!(
            error,
            DomainError::Kafka(error) if error.category == KafkaErrorCategory::Tls
        ));
    }

    #[cfg(not(feature = "kafka-sasl"))]
    #[test]
    fn sasl_connection_requires_build_feature() {
        let mut config = KafkaClusterConfig::new("secure", vec!["broker:9092".into()]);
        config.security_protocol = ramag_domain::entities::KafkaSecurityProtocol::SaslPlaintext;
        config.sasl_mechanism = Some(ramag_domain::entities::KafkaSaslMechanism::Plain);
        config.sasl_username = Some("user".into());
        config.sasl_password = Some("password".into());
        let result = RdkafkaDriver::new().test_connection_blocking(&config);
        assert!(result.is_err());
        let error = match result {
            Ok(()) => return,
            Err(error) => error,
        };
        assert!(matches!(
            error,
            DomainError::Kafka(error) if error.category == KafkaErrorCategory::Unsupported
        ));
    }

    #[cfg(feature = "cmake-build")]
    #[test]
    fn connection_error_category_is_preserved() {
        let error = errors::map_kafka_error(
            rdkafka::error::KafkaError::MetadataFetch(
                rdkafka::error::RDKafkaErrorCode::SaslAuthenticationFailed,
            ),
            "测试 Kafka 连接",
        );
        assert!(matches!(
            error,
            DomainError::Kafka(ref error)
                if error.category == KafkaErrorCategory::Authentication
                    && error.operation == "测试 Kafka 连接"
        ));
    }

    #[cfg(not(feature = "cmake-build"))]
    #[test]
    fn default_build_reports_missing_native_client_without_network_access() {
        let config = KafkaClusterConfig::new("local", vec!["broker:9092".into()]);
        let result = RdkafkaDriver::new().test_connection_blocking(&config);
        assert!(matches!(
            result,
            Err(DomainError::Kafka(error))
                if error.category == KafkaErrorCategory::Unsupported
                    && error.safe_message.contains("cmake-build")
        ));
    }
}
