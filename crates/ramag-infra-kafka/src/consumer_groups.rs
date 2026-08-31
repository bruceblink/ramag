use super::*;
use ramag_domain::entities::{
    KafkaConsumerGroup, KafkaConsumerGroupOffset, KafkaConsumerMember,
    KafkaConsumerPartitionAssignment, MAX_KAFKA_CONSUMER_GROUPS, MAX_KAFKA_GROUP_ASSIGNMENT_BYTES,
    MAX_KAFKA_GROUP_MEMBERS, MAX_KAFKA_GROUP_OFFSETS, MAX_KAFKA_PARTITIONS,
};
use rdkafka::topic_partition_list::{Offset, TopicPartitionList};
use std::collections::HashMap;

impl RdkafkaDriver {
    /// 读取消费者组、成员分配和已提交 Offset；查询客户端不会提交或改变任何 Offset。
    pub(super) fn list_consumer_groups_blocking(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<Vec<KafkaConsumerGroup>> {
        Self::ensure_build_features(config)?;
        let topics = self.list_topics_blocking(config)?;
        let browser = self.create_consumer(config)?;
        let group_list = browser
            .fetch_group_list(None, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "读取 Kafka 消费者组"))?;
        if group_list.groups().len() > MAX_KAFKA_CONSUMER_GROUPS {
            return Err(DomainError::InvalidConfig(format!(
                "Kafka 消费者组数量超过 {MAX_KAFKA_CONSUMER_GROUPS} 个上限"
            )));
        }

        let mut partitions = TopicPartitionList::new();
        let mut high_watermarks = HashMap::new();
        for topic in &topics {
            for partition in &topic.partitions {
                partitions.add_partition(&topic.name, partition.id);
                if partitions.count() > MAX_KAFKA_GROUP_OFFSETS {
                    return Err(DomainError::InvalidConfig(format!(
                        "Kafka 消费者组 Offset 查询范围超过 {MAX_KAFKA_GROUP_OFFSETS} 个上限"
                    )));
                }
                if let Some(high) = partition.high_watermark {
                    high_watermarks.insert((topic.name.clone(), partition.id), high);
                }
            }
        }
        let mut groups = Vec::with_capacity(group_list.groups().len());
        let mut total_offsets = 0usize;
        for group in group_list.groups() {
            if group.members().len() > MAX_KAFKA_GROUP_MEMBERS {
                return Err(DomainError::InvalidConfig(format!(
                    "消费者组成员数量超过 {MAX_KAFKA_GROUP_MEMBERS} 个上限：{}",
                    group.name()
                )));
            }
            let members = group
                .members()
                .iter()
                .map(|member| KafkaConsumerMember {
                    member_id: member.id().to_owned(),
                    client_id: member.client_id().to_owned(),
                    client_host: (!member.client_host().is_empty())
                        .then(|| member.client_host().to_owned()),
                    assigned_partitions: member
                        .assignment()
                        .map(decode_member_assignment)
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            let offsets =
                fetch_group_offsets(self, config, group.name(), &partitions, &high_watermarks)?;
            total_offsets = total_offsets.saturating_add(offsets.len());
            if total_offsets > MAX_KAFKA_GROUP_OFFSETS {
                return Err(DomainError::InvalidConfig(format!(
                    "消费者组 Offset 快照超过 {MAX_KAFKA_GROUP_OFFSETS} 个上限"
                )));
            }
            groups.push(KafkaConsumerGroup {
                group_id: group.name().to_owned(),
                state: (!group.state().is_empty()).then(|| group.state().to_owned()),
                protocol: (!group.protocol().is_empty()).then(|| group.protocol().to_owned()),
                members,
                offsets,
            });
        }
        groups.sort_by(|left, right| left.group_id.cmp(&right.group_id));
        Ok(groups)
    }

    pub(super) fn create_group_consumer(
        &self,
        config: &KafkaClusterConfig,
        group_id: &str,
    ) -> Result<BaseConsumer> {
        Self::ensure_build_features(config)?;
        let mut client_config = config::build_client_config(config, self.request_timeout)?;
        client_config.set("group.id", group_id);
        client_config
            .create()
            .map_err(|error| errors::map_kafka_error(error, "创建 Kafka Offset 查询客户端"))
    }
}

fn fetch_group_offsets(
    driver: &RdkafkaDriver,
    config: &KafkaClusterConfig,
    group_id: &str,
    partitions: &TopicPartitionList,
    high_watermarks: &HashMap<(String, i32), i64>,
) -> Result<Vec<KafkaConsumerGroupOffset>> {
    if partitions.count() == 0 {
        return Ok(Vec::new());
    }
    let consumer = driver.create_group_consumer(config, group_id)?;
    let committed = consumer
        .committed_offsets(partitions.clone(), driver.request_timeout)
        .map_err(|error| errors::map_kafka_error(error, "读取 Kafka 消费者组 Offset"))?;
    let mut offsets = Vec::new();
    for element in committed.elements() {
        element
            .error()
            .map_err(|error| errors::map_kafka_error(error, "读取 Kafka 消费者组 Offset"))?;
        let Offset::Offset(committed_offset) = element.offset() else {
            continue;
        };
        if committed_offset < 0 {
            continue;
        }
        let key = (element.topic().to_owned(), element.partition());
        let end_offset = high_watermarks
            .get(&key)
            .copied()
            .filter(|end| *end >= committed_offset);
        let lag = end_offset.map(|end| end.saturating_sub(committed_offset));
        offsets.push(KafkaConsumerGroupOffset {
            topic: key.0,
            partition: key.1,
            committed_offset: Some(committed_offset),
            end_offset,
            lag,
        });
        if offsets.len() > MAX_KAFKA_GROUP_OFFSETS {
            return Err(DomainError::InvalidConfig(format!(
                "消费者组 Offset 数量超过 {MAX_KAFKA_GROUP_OFFSETS} 个上限：{group_id}"
            )));
        }
    }
    offsets.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then(left.partition.cmp(&right.partition))
    });
    Ok(offsets)
}

/// 解码 Kafka ConsumerProtocolAssignment；损坏或超限的协议数据会安全地显示为空分配。
pub(super) fn decode_member_assignment(bytes: &[u8]) -> Vec<KafkaConsumerPartitionAssignment> {
    if bytes.len() > MAX_KAFKA_GROUP_ASSIGNMENT_BYTES {
        return Vec::new();
    }
    let mut reader = AssignmentReader { bytes, position: 0 };
    let Some(_version) = reader.read_i16() else {
        return Vec::new();
    };
    let Some(topic_count) = reader.read_i32() else {
        return Vec::new();
    };
    if topic_count < 0 || usize::try_from(topic_count).unwrap_or(usize::MAX) > MAX_KAFKA_PARTITIONS
    {
        return Vec::new();
    }
    let mut assignments = Vec::new();
    for _ in 0..topic_count {
        let Some(topic) = reader.read_string() else {
            return Vec::new();
        };
        let Some(partition_count) = reader.read_i32() else {
            return Vec::new();
        };
        if partition_count < 0
            || usize::try_from(partition_count).unwrap_or(usize::MAX) > MAX_KAFKA_PARTITIONS
        {
            return Vec::new();
        }
        for _ in 0..partition_count {
            let Some(partition) = reader.read_i32() else {
                return Vec::new();
            };
            if partition < 0 || assignments.len() >= MAX_KAFKA_PARTITIONS {
                return Vec::new();
            }
            assignments.push(KafkaConsumerPartitionAssignment {
                topic: topic.clone(),
                partition,
            });
        }
    }
    let Some(user_data_length) = reader.read_i32() else {
        return Vec::new();
    };
    if user_data_length < -1 {
        return Vec::new();
    }
    if user_data_length >= 0
        && !reader.skip(usize::try_from(user_data_length).unwrap_or(usize::MAX))
    {
        return Vec::new();
    }
    if reader.remaining() != 0 {
        return Vec::new();
    }
    assignments
}

struct AssignmentReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl AssignmentReader<'_> {
    fn read_i16(&mut self) -> Option<i16> {
        let bytes = self.read_exact(2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_i32(&mut self) -> Option<i32> {
        let bytes = self.read_exact(4)?;
        Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_string(&mut self) -> Option<String> {
        let length = self.read_i16()?;
        if length <= 0 {
            return None;
        }
        let bytes = self.read_exact(usize::try_from(length).ok()?)?;
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn read_exact(&mut self, length: usize) -> Option<&[u8]> {
        let end = self.position.checked_add(length)?;
        let bytes = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(bytes)
    }

    fn skip(&mut self, length: usize) -> bool {
        self.read_exact(length).is_some()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }
}
