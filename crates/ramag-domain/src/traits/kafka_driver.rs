//! Kafka 只读与管理能力的领域边界。

use async_trait::async_trait;

use crate::entities::{
    KafkaClusterConfig, KafkaClusterMetadata, KafkaConfigResource, KafkaConfigResourceType,
    KafkaConfigUpdateRequest, KafkaMessagePage, KafkaMessageQuery, KafkaMessageSearchQuery,
    KafkaTopic, KafkaTopicCreateRequest, KafkaTopicPartitionExpansion,
};
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

    async fn list_topics(&self, _config: &KafkaClusterConfig) -> Result<Vec<KafkaTopic>> {
        Err(crate::error::DomainError::NotImplemented(
            "list_topics".into(),
        ))
    }

    async fn read_messages(
        &self,
        _config: &KafkaClusterConfig,
        _query: &KafkaMessageQuery,
    ) -> Result<KafkaMessagePage> {
        Err(crate::error::DomainError::NotImplemented(
            "read_messages".into(),
        ))
    }

    async fn search_messages(
        &self,
        _config: &KafkaClusterConfig,
        _query: &KafkaMessageSearchQuery,
    ) -> Result<KafkaMessagePage> {
        Err(crate::error::DomainError::NotImplemented(
            "search_messages".into(),
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

    async fn create_topic(
        &self,
        _config: &KafkaClusterConfig,
        _request: &KafkaTopicCreateRequest,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "create_topic".into(),
        ))
    }

    async fn delete_topic(&self, _config: &KafkaClusterConfig, _topic: &str) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "delete_topic".into(),
        ))
    }

    async fn increase_topic_partitions(
        &self,
        _config: &KafkaClusterConfig,
        _request: &KafkaTopicPartitionExpansion,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "increase_topic_partitions".into(),
        ))
    }

    async fn describe_configs(
        &self,
        _config: &KafkaClusterConfig,
        _resource_type: KafkaConfigResourceType,
        _resource_name: &str,
    ) -> Result<KafkaConfigResource> {
        Err(crate::error::DomainError::NotImplemented(
            "describe_configs".into(),
        ))
    }

    async fn update_config(
        &self,
        _config: &KafkaClusterConfig,
        _request: &KafkaConfigUpdateRequest,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "update_config".into(),
        ))
    }
}
