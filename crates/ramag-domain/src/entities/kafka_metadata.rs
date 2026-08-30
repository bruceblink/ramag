use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::kafka_validation::{validate_optional_single_line, validate_required_text};
use super::{
    MAX_KAFKA_BOOTSTRAP_SERVER_BYTES, MAX_KAFKA_BROKERS, MAX_KAFKA_CLUSTER_ID_BYTES,
    MAX_KAFKA_PARTITIONS, MAX_KAFKA_REPLICAS, MAX_KAFKA_VERSION_BYTES, validate_kafka_topic_name,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaClusterMetadata {
    pub cluster_id: Option<String>,
    pub controller_id: Option<i32>,
    pub brokers: Vec<KafkaBroker>,
    #[serde(default)]
    pub kafka_version: Option<String>,
}

impl KafkaClusterMetadata {
    /// 校验一次元数据快照，拒绝重复 Broker 和异常大的响应集合。
    pub fn validate(&self) -> Result<(), String> {
        if self.brokers.is_empty() {
            return Err("Kafka 集群元数据至少需要一个 Broker".into());
        }
        if self.brokers.len() > MAX_KAFKA_BROKERS {
            return Err(format!("Broker 数量超过 {MAX_KAFKA_BROKERS} 个上限"));
        }
        validate_optional_single_line(
            "Cluster ID",
            self.cluster_id.as_deref(),
            MAX_KAFKA_CLUSTER_ID_BYTES,
        )?;
        validate_optional_single_line(
            "Kafka 版本",
            self.kafka_version.as_deref(),
            MAX_KAFKA_VERSION_BYTES,
        )?;
        if self.controller_id.is_some_and(|id| id < 0) {
            return Err("Controller ID 不能为负数".into());
        }

        let mut ids = HashSet::with_capacity(self.brokers.len());
        for broker in &self.brokers {
            broker.validate()?;
            if !ids.insert(broker.id) {
                return Err(format!("Broker ID 重复：{}", broker.id));
            }
        }
        if let Some(controller_id) = self.controller_id
            && !ids.contains(&controller_id)
        {
            return Err(format!(
                "Controller ID 不存在于 Broker 列表：{controller_id}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaBroker {
    pub id: i32,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub rack: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub is_controller: bool,
}

impl KafkaBroker {
    /// 校验 Broker 的网络地址和元数据文本，确保 UI 不接收控制字符。
    pub fn validate(&self) -> Result<(), String> {
        if self.id < 0 {
            return Err("Broker ID 不能为负数".into());
        }
        validate_required_text(
            "Broker 主机地址",
            &self.host,
            MAX_KAFKA_BOOTSTRAP_SERVER_BYTES,
        )?;
        if self.host.chars().any(char::is_whitespace) {
            return Err("Broker 主机地址不能包含空白字符".into());
        }
        if self.port == 0 {
            return Err("Broker 端口必须是 1 - 65535".into());
        }
        validate_optional_single_line(
            "Broker Rack",
            self.rack.as_deref(),
            MAX_KAFKA_VERSION_BYTES,
        )?;
        validate_optional_single_line(
            "Broker 版本",
            self.version.as_deref(),
            MAX_KAFKA_VERSION_BYTES,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTopic {
    pub name: String,
    #[serde(default)]
    pub partitions: Vec<KafkaPartition>,
    #[serde(default)]
    pub internal: bool,
}

impl KafkaTopic {
    /// 校验 Topic 名称、Partition 数量和 Partition ID 唯一性。
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_topic_name(&self.name)?;
        if self.partitions.len() > MAX_KAFKA_PARTITIONS {
            return Err(format!(
                "Topic Partition 数量超过 {MAX_KAFKA_PARTITIONS} 个上限"
            ));
        }
        let mut ids = HashSet::with_capacity(self.partitions.len());
        for partition in &self.partitions {
            partition.validate()?;
            if !ids.insert(partition.id) {
                return Err(format!("Topic Partition ID 重复：{}", partition.id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaPartition {
    pub id: i32,
    #[serde(default)]
    pub leader: Option<i32>,
    #[serde(default)]
    pub replicas: Vec<i32>,
    #[serde(default)]
    pub isr: Vec<i32>,
    #[serde(default)]
    pub low_watermark: Option<i64>,
    #[serde(default)]
    pub high_watermark: Option<i64>,
}

impl KafkaPartition {
    /// 校验副本集合和首尾 Offset，避免出现无法解释的分区快照。
    pub fn validate(&self) -> Result<(), String> {
        if self.id < 0 {
            return Err("Partition ID 不能为负数".into());
        }
        if self.leader.is_some_and(|id| id < 0) {
            return Err("Partition Leader ID 不能为负数".into());
        }
        validate_broker_ids("副本", &self.replicas)?;
        validate_broker_ids("ISR", &self.isr)?;
        if self.isr.iter().any(|id| !self.replicas.contains(id)) {
            return Err("ISR 必须是副本列表的子集".into());
        }
        if let (Some(low), Some(high)) = (self.low_watermark, self.high_watermark) {
            if low < 0 || high < 0 || low > high {
                return Err("Partition 首尾 Offset 无效".into());
            }
        } else if self.low_watermark.is_some_and(|offset| offset < 0)
            || self.high_watermark.is_some_and(|offset| offset < 0)
        {
            return Err("Partition Offset 不能为负数".into());
        }
        Ok(())
    }
}

fn validate_broker_ids(label: &str, ids: &[i32]) -> Result<(), String> {
    if ids.len() > MAX_KAFKA_REPLICAS {
        return Err(format!("{label}数量超过 {MAX_KAFKA_REPLICAS} 个上限"));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    for id in ids {
        if *id < 0 {
            return Err(format!("{label} Broker ID 不能为负数"));
        }
        if !seen.insert(id) {
            return Err(format!("{label} Broker ID 重复：{id}"));
        }
    }
    Ok(())
}
