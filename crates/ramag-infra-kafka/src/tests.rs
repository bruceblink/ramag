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
