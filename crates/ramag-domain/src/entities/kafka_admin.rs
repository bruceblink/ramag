use serde::{Deserialize, Serialize};

use super::kafka_validation::validate_kafka_managed_topic_name;
use super::{MAX_KAFKA_PARTITIONS, MAX_KAFKA_REPLICAS};

/// 创建 Topic 时提交给 Kafka Admin API 的有限请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTopicCreateRequest {
    pub name: String,
    pub partitions: usize,
    pub replication_factor: usize,
}

impl KafkaTopicCreateRequest {
    pub fn new(name: impl Into<String>, partitions: usize, replication_factor: usize) -> Self {
        Self {
            name: name.into(),
            partitions,
            replication_factor,
        }
    }

    /// 校验创建请求，避免把空数量、内部 Topic 或超大请求交给 Broker。
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_managed_topic_name(&self.name)?;
        validate_positive_limit(
            "Topic Partition 数量",
            self.partitions,
            MAX_KAFKA_PARTITIONS,
        )?;
        validate_positive_limit(
            "Topic 副本因子",
            self.replication_factor,
            MAX_KAFKA_REPLICAS,
        )?;
        Ok(())
    }
}

/// 增加 Topic Partition 时使用的目标总数；Kafka 不允许减少 Partition。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTopicPartitionExpansion {
    pub name: String,
    pub total_partitions: usize,
}

impl KafkaTopicPartitionExpansion {
    pub fn new(name: impl Into<String>, total_partitions: usize) -> Self {
        Self {
            name: name.into(),
            total_partitions,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_managed_topic_name(&self.name)?;
        validate_positive_limit(
            "Topic Partition 总数",
            self.total_partitions,
            MAX_KAFKA_PARTITIONS,
        )?;
        Ok(())
    }
}

fn validate_positive_limit(label: &str, value: usize, max: usize) -> Result<(), String> {
    if value == 0 {
        return Err(format!("{label}必须大于 0"));
    }
    if value > max {
        return Err(format!("{label}不能超过 {max} 个"));
    }
    Ok(())
}
