//! 将 rdkafka 错误映射为不泄露凭据的结构化领域错误。

use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory};
use rdkafka::error::{KafkaError as RdkafkaError, RDKafkaErrorCode};

/// 根据底层错误码返回用户可区分的 Kafka 错误分类。
pub fn map_kafka_error(error: RdkafkaError, operation: &'static str) -> DomainError {
    let category = if matches!(&error, RdkafkaError::Canceled) {
        KafkaErrorCategory::Cancelled
    } else {
        category_for_code(code_of(&error), &error)
    };
    let code = code_of(&error)
        .map(|code| code.to_string())
        .unwrap_or_else(|| "client_error".into());
    DomainError::Kafka(
        KafkaError::new(
            category,
            operation,
            format!("Kafka 操作失败：{operation}（{code}）"),
        )
        .retryable(matches!(
            category,
            KafkaErrorCategory::Network | KafkaErrorCategory::Timeout
        )),
    )
}

/// 提取底层错误码；客户端配置和创建错误本身可能没有 Broker 错误码。
fn code_of(error: &RdkafkaError) -> Option<RDKafkaErrorCode> {
    match error {
        RdkafkaError::AdminOp(code)
        | RdkafkaError::ConsumerCommit(code)
        | RdkafkaError::ConsumerQueueClose(code)
        | RdkafkaError::Flush(code)
        | RdkafkaError::Global(code)
        | RdkafkaError::GroupListFetch(code)
        | RdkafkaError::MessageConsumption(code)
        | RdkafkaError::MessageConsumptionFatal(code)
        | RdkafkaError::MessageProduction(code)
        | RdkafkaError::MetadataFetch(code)
        | RdkafkaError::OffsetFetch(code)
        | RdkafkaError::Rebalance(code)
        | RdkafkaError::SetPartitionOffset(code)
        | RdkafkaError::StoreOffset(code)
        | RdkafkaError::MockCluster(code) => Some(*code),
        RdkafkaError::Transaction(error) => Some(error.code()),
        _ => None,
    }
}

/// 把底层错误码归并为 UI 和重试策略可使用的稳定分类。
fn category_for_code(code: Option<RDKafkaErrorCode>, error: &RdkafkaError) -> KafkaErrorCategory {
    if matches!(error, RdkafkaError::ClientConfig(..)) {
        return KafkaErrorCategory::InvalidConfig;
    }
    match code {
        Some(
            RDKafkaErrorCode::Authentication
            | RDKafkaErrorCode::SaslAuthenticationFailed
            | RDKafkaErrorCode::IllegalSASLState,
        ) => KafkaErrorCategory::Authentication,
        Some(RDKafkaErrorCode::SSL) => KafkaErrorCategory::Tls,
        Some(
            RDKafkaErrorCode::OperationTimedOut
            | RDKafkaErrorCode::RequestTimedOut
            | RDKafkaErrorCode::TimedOutQueue,
        ) => KafkaErrorCategory::Timeout,
        Some(
            RDKafkaErrorCode::TopicAuthorizationFailed
            | RDKafkaErrorCode::GroupAuthorizationFailed
            | RDKafkaErrorCode::ClusterAuthorizationFailed
            | RDKafkaErrorCode::TransactionalIdAuthorizationFailed
            | RDKafkaErrorCode::DelegationTokenAuthorizationFailed,
        ) => KafkaErrorCategory::PermissionDenied,
        Some(
            RDKafkaErrorCode::UnknownTopic
            | RDKafkaErrorCode::UnknownPartition
            | RDKafkaErrorCode::UnknownTopicOrPartition
            | RDKafkaErrorCode::NoEnt
            | RDKafkaErrorCode::GroupIdNotFound,
        ) => KafkaErrorCategory::NotFound,
        Some(
            RDKafkaErrorCode::UnsupportedSASLMechanism
            | RDKafkaErrorCode::UnsupportedFeature
            | RDKafkaErrorCode::UnsupportedVersion
            | RDKafkaErrorCode::NotImplemented
            | RDKafkaErrorCode::SecurityDisabled,
        ) => KafkaErrorCategory::Unsupported,
        Some(
            RDKafkaErrorCode::BrokerTransportFailure
            | RDKafkaErrorCode::Resolve
            | RDKafkaErrorCode::AllBrokersDown
            | RDKafkaErrorCode::BrokerNotAvailable
            | RDKafkaErrorCode::NetworkException,
        ) => KafkaErrorCategory::Network,
        Some(
            RDKafkaErrorCode::InvalidArgument
            | RDKafkaErrorCode::InvalidConfig
            | RDKafkaErrorCode::InvalidTopic
            | RDKafkaErrorCode::InvalidPartitions
            | RDKafkaErrorCode::InvalidReplicationFactor,
        ) => KafkaErrorCategory::InvalidConfig,
        Some(RDKafkaErrorCode::BadMessage | RDKafkaErrorCode::UnknownProtocol) => {
            KafkaErrorCategory::Protocol
        }
        _ => KafkaErrorCategory::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_error_categories_without_raw_credentials() {
        let cases = [
            (
                RdkafkaError::MetadataFetch(RDKafkaErrorCode::SSL),
                KafkaErrorCategory::Tls,
            ),
            (
                RdkafkaError::MetadataFetch(RDKafkaErrorCode::RequestTimedOut),
                KafkaErrorCategory::Timeout,
            ),
            (
                RdkafkaError::MetadataFetch(RDKafkaErrorCode::TopicAuthorizationFailed),
                KafkaErrorCategory::PermissionDenied,
            ),
        ];
        for (error, category) in cases {
            let mapped = map_kafka_error(error, "test");
            assert!(matches!(mapped, DomainError::Kafka(ref error) if error.category == category));
            assert!(!mapped.to_string().contains("password"));
        }
    }
}
