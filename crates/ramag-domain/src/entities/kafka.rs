//! Kafka 领域实体模块的公开入口。

#[path = "kafka_acl.rs"]
mod kafka_acl;
#[path = "kafka_admin.rs"]
mod kafka_admin;
#[path = "kafka_config.rs"]
mod kafka_config;
#[path = "kafka_consumer.rs"]
mod kafka_consumer;
#[path = "kafka_message.rs"]
mod kafka_message;
#[path = "kafka_metadata.rs"]
mod kafka_metadata;
#[path = "kafka_validation.rs"]
mod kafka_validation;

pub use kafka_acl::{
    KafkaAcl, KafkaAclOperation, KafkaAclPatternType, KafkaAclPermission, KafkaAclResourceType,
};
pub use kafka_admin::{
    KafkaConfigEntry, KafkaConfigResource, KafkaConfigResourceType, KafkaConfigSource,
    KafkaConfigUpdateOperation, KafkaConfigUpdateRequest, KafkaTopicCreateRequest,
    KafkaTopicPartitionExpansion,
};
pub use kafka_config::{
    KafkaClusterConfig, KafkaClusterId, KafkaReadOnlyState, KafkaSaslMechanism,
    KafkaSecurityProtocol, KafkaTlsConfig,
};
pub use kafka_consumer::{
    KafkaConsumerGroup, KafkaConsumerGroupOffset, KafkaConsumerMember,
    KafkaConsumerPartitionAssignment,
};
pub use kafka_message::{
    KafkaMessageHeader, KafkaMessagePage, KafkaMessageQuery, KafkaMessageRecord,
    KafkaMessageSearchField, KafkaMessageSearchQuery, KafkaTextPreview,
};
pub use kafka_metadata::{KafkaBroker, KafkaClusterMetadata, KafkaPartition, KafkaTopic};
pub use kafka_validation::{
    preview_bytes, validate_kafka_bootstrap_server, validate_kafka_managed_topic_name,
    validate_kafka_topic_name,
};

pub const MAX_KAFKA_CLUSTER_NAME_BYTES: usize = 256;
pub const MAX_KAFKA_CLUSTERS: usize = 2_048;
pub const MAX_KAFKA_BOOTSTRAP_SERVERS: usize = 32;
pub const MAX_KAFKA_BOOTSTRAP_SERVER_BYTES: usize = 1024;
pub const MAX_KAFKA_BOOTSTRAP_SERVERS_BYTES: usize = 16 * 1024;
pub const MAX_KAFKA_CLIENT_ID_BYTES: usize = 256;
pub const MAX_KAFKA_SASL_USERNAME_BYTES: usize = 4 * 1024;
pub const MAX_KAFKA_SASL_PASSWORD_BYTES: usize = 64 * 1024;
pub const MAX_KAFKA_REMARK_BYTES: usize = 16 * 1024;
pub const MAX_KAFKA_TLS_PATH_BYTES: usize = 32 * 1024;

/// Kafka 的 Topic 名称上限来自 Broker 的协议约束。
pub const MAX_KAFKA_TOPIC_NAME_BYTES: usize = 249;
pub const MAX_KAFKA_CLUSTER_ID_BYTES: usize = 1024;
pub const MAX_KAFKA_VERSION_BYTES: usize = 256;
pub const MAX_KAFKA_BROKERS: usize = 10_000;
pub const MAX_KAFKA_TOPICS: usize = 100_000;
pub const MAX_KAFKA_PARTITIONS: usize = 1_000_000;
pub const MAX_KAFKA_REPLICAS: usize = 1_000;

pub const MAX_KAFKA_MESSAGE_HEADERS: usize = 1_000;
pub const MAX_KAFKA_HEADER_KEY_BYTES: usize = 1024;
pub const MAX_KAFKA_MESSAGE_PREVIEW_BYTES: usize = 64 * 1024;

pub const MAX_KAFKA_QUERY_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_KAFKA_QUERY_PARTITIONS: usize = 256;
pub const MAX_KAFKA_SCAN_RECORDS: usize = 50_000;
pub const MAX_KAFKA_SCAN_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_KAFKA_SCAN_SECONDS: u32 = 300;
pub const MAX_KAFKA_CONCURRENT_PARTITIONS: usize = 32;

pub const MAX_KAFKA_CONSUMER_GROUPS: usize = 100_000;
pub const MAX_KAFKA_GROUP_MEMBERS: usize = 10_000;
pub const MAX_KAFKA_GROUP_OFFSETS: usize = 1_000_000;

pub const MAX_KAFKA_ACLS: usize = 100_000;
pub const MAX_KAFKA_PRINCIPAL_BYTES: usize = 1024;
pub const MAX_KAFKA_ACL_HOST_BYTES: usize = 1024;
pub const MAX_KAFKA_ACL_RESOURCE_NAME_BYTES: usize = 1024;

pub const MAX_KAFKA_CONFIG_ENTRIES: usize = 20_000;
pub const MAX_KAFKA_CONFIG_KEY_BYTES: usize = 256;
pub const MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES: usize = 249;
pub const MAX_KAFKA_CONFIG_VALUE_BYTES: usize = 64 * 1024;

pub const DEFAULT_KAFKA_MAX_RECORDS: usize = 1_000;
pub const DEFAULT_KAFKA_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_KAFKA_MAX_SCAN_SECONDS: u32 = 30;
pub const DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS: usize = 4;
