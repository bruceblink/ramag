//! Kafka 只读与管理能力的领域边界。

use async_trait::async_trait;

use crate::entities::{KafkaClusterConfig, KafkaClusterMetadata};
use crate::error::Result;

/// Kafka 读取端口；不会提交 Offset，也不修改集群状态。
#[async_trait]
pub trait KafkaDriver: Send + Sync {
    fn name(&self) -> &'static str {
        "kafka"
    }

    async fn test_connection(&self, config: &KafkaClusterConfig) -> Result<()>;

    async fn cluster_metadata(&self, _config: &KafkaClusterConfig) -> Result<KafkaClusterMetadata> {
        Err(crate::error::DomainError::NotImplemented(
            "cluster_metadata".into(),
        ))
    }
}

/// Kafka 管理端口；与只读消息浏览能力分开注入，后续承载确认过的变更操作。
#[async_trait]
pub trait KafkaAdminDriver: Send + Sync {
    async fn test_admin_connection(&self, _config: &KafkaClusterConfig) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "test_admin_connection".into(),
        ))
    }
}
