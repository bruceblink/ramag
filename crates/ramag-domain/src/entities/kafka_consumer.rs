use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::kafka_validation::{
    validate_optional_offset, validate_optional_single_line, validate_required_text,
};
use super::{
    MAX_KAFKA_ACL_RESOURCE_NAME_BYTES, MAX_KAFKA_BOOTSTRAP_SERVER_BYTES, MAX_KAFKA_CLIENT_ID_BYTES,
    MAX_KAFKA_GROUP_MEMBERS, MAX_KAFKA_GROUP_OFFSETS, MAX_KAFKA_PARTITIONS,
    MAX_KAFKA_VERSION_BYTES, validate_kafka_topic_name,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConsumerGroup {
    pub group_id: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub members: Vec<KafkaConsumerMember>,
    #[serde(default)]
    pub offsets: Vec<KafkaConsumerGroupOffset>,
}

impl KafkaConsumerGroup {
    /// 校验消费者组、成员和 Offset 快照的数量与唯一性。
    pub fn validate(&self) -> Result<(), String> {
        validate_required_text(
            "消费者组 ID",
            &self.group_id,
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
        )?;
        validate_optional_single_line(
            "消费者组状态",
            self.state.as_deref(),
            MAX_KAFKA_VERSION_BYTES,
        )?;
        validate_optional_single_line(
            "消费者组协议",
            self.protocol.as_deref(),
            MAX_KAFKA_VERSION_BYTES,
        )?;
        if self.members.len() > MAX_KAFKA_GROUP_MEMBERS {
            return Err(format!(
                "消费者组成员数量超过 {MAX_KAFKA_GROUP_MEMBERS} 个上限"
            ));
        }
        if self.offsets.len() > MAX_KAFKA_GROUP_OFFSETS {
            return Err(format!(
                "消费者组 Offset 数量超过 {MAX_KAFKA_GROUP_OFFSETS} 个上限"
            ));
        }
        let mut member_ids = HashSet::with_capacity(self.members.len());
        for member in &self.members {
            member.validate()?;
            if !member_ids.insert(member.member_id.as_str()) {
                return Err(format!("消费者组成员 ID 重复：{}", member.member_id));
            }
        }
        let mut offsets = HashSet::with_capacity(self.offsets.len());
        for offset in &self.offsets {
            offset.validate()?;
            if !offsets.insert((&offset.topic, offset.partition)) {
                return Err(format!(
                    "消费者组 Topic/Partition Offset 重复：{}/{}",
                    offset.topic, offset.partition
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConsumerMember {
    pub member_id: String,
    pub client_id: String,
    #[serde(default)]
    pub client_host: Option<String>,
    #[serde(default)]
    pub assigned_partitions: Vec<KafkaConsumerPartitionAssignment>,
}

impl KafkaConsumerMember {
    /// 校验成员标识和分配的 Topic/Partition，确保列表可安全展示。
    pub fn validate(&self) -> Result<(), String> {
        validate_required_text("消费者成员 ID", &self.member_id, MAX_KAFKA_CLIENT_ID_BYTES)?;
        validate_required_text(
            "消费者 Client ID",
            &self.client_id,
            MAX_KAFKA_CLIENT_ID_BYTES,
        )?;
        validate_optional_single_line(
            "消费者客户端地址",
            self.client_host.as_deref(),
            MAX_KAFKA_BOOTSTRAP_SERVER_BYTES,
        )?;
        validate_assignments(&self.assigned_partitions)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KafkaConsumerPartitionAssignment {
    pub topic: String,
    pub partition: i32,
}

impl KafkaConsumerPartitionAssignment {
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_topic_name(&self.topic)?;
        if self.partition < 0 {
            return Err("消费者分配的 Partition 不能为负数".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConsumerGroupOffset {
    pub topic: String,
    pub partition: i32,
    #[serde(default)]
    pub committed_offset: Option<i64>,
    #[serde(default)]
    pub end_offset: Option<i64>,
    #[serde(default)]
    pub lag: Option<i64>,
}

impl KafkaConsumerGroupOffset {
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_topic_name(&self.topic)?;
        if self.partition < 0 {
            return Err("消费者组 Offset 的 Partition 不能为负数".into());
        }
        validate_optional_offset(self.committed_offset)?;
        validate_optional_offset(self.end_offset)?;
        if self.lag.is_some_and(|lag| lag < -1) {
            return Err("消费者组 Lag 不能小于 -1".into());
        }
        if let (Some(committed), Some(end)) = (self.committed_offset, self.end_offset)
            && committed > end
        {
            return Err("消费者组提交 Offset 不能超过末尾 Offset".into());
        }
        Ok(())
    }
}

fn validate_assignments(assignments: &[KafkaConsumerPartitionAssignment]) -> Result<(), String> {
    if assignments.len() > MAX_KAFKA_PARTITIONS {
        return Err(format!(
            "消费者成员分配的 Partition 数量超过 {MAX_KAFKA_PARTITIONS} 个上限"
        ));
    }
    let mut seen = HashSet::with_capacity(assignments.len());
    for assignment in assignments {
        assignment.validate()?;
        if !seen.insert((&assignment.topic, assignment.partition)) {
            return Err(format!(
                "消费者成员分配重复：{}/{}",
                assignment.topic, assignment.partition
            ));
        }
    }
    Ok(())
}
