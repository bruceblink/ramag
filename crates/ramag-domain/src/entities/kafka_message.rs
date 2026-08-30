use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::kafka_validation::{preview_bytes, validate_optional_offset, validate_protocol_text};
use super::{
    DEFAULT_KAFKA_MAX_BYTES, DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS, DEFAULT_KAFKA_MAX_RECORDS,
    DEFAULT_KAFKA_MAX_SCAN_SECONDS, MAX_KAFKA_CONCURRENT_PARTITIONS, MAX_KAFKA_HEADER_KEY_BYTES,
    MAX_KAFKA_MESSAGE_HEADERS, MAX_KAFKA_QUERY_PARTITIONS, MAX_KAFKA_QUERY_TEXT_BYTES,
    MAX_KAFKA_SCAN_BYTES, MAX_KAFKA_SCAN_RECORDS, MAX_KAFKA_SCAN_SECONDS,
    validate_kafka_topic_name,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaMessageHeader {
    pub key: String,
    pub value: Option<Vec<u8>>,
}

impl KafkaMessageHeader {
    /// 校验 Header 名称；Header 值保留原始字节，不在领域层猜测编码。
    pub fn validate(&self) -> Result<(), String> {
        if self.key.trim().is_empty() {
            return Err("Header Key 不能为空".into());
        }
        validate_protocol_text("Header Key", &self.key, MAX_KAFKA_HEADER_KEY_BYTES)?;
        if self.key.chars().any(char::is_control) {
            return Err("Header Key 不能包含控制字符".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTextPreview {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaMessageRecord {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    #[serde(default)]
    pub headers: Vec<KafkaMessageHeader>,
}

impl KafkaMessageRecord {
    /// 校验消息定位字段和 Header 数量；Key/Value 字节保持原样，不因编码失败而丢弃。
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_topic_name(&self.topic)?;
        if self.partition < 0 {
            return Err("消息 Partition 不能为负数".into());
        }
        if self.offset < 0 {
            return Err("消息 Offset 不能为负数".into());
        }
        if self.headers.len() > MAX_KAFKA_MESSAGE_HEADERS {
            return Err(format!(
                "消息 Header 数量超过 {MAX_KAFKA_MESSAGE_HEADERS} 个上限"
            ));
        }
        for header in &self.headers {
            header.validate()?;
        }
        Ok(())
    }

    pub fn key_preview(&self, max_bytes: usize) -> Option<KafkaTextPreview> {
        self.key
            .as_deref()
            .map(|bytes| preview_bytes(bytes, max_bytes))
    }

    pub fn value_preview(&self, max_bytes: usize) -> Option<KafkaTextPreview> {
        self.value
            .as_deref()
            .map(|bytes| preview_bytes(bytes, max_bytes))
    }

    /// 返回消息在当前领域对象中直接持有的字节数，供 UI 和任务预算使用。
    pub fn retained_bytes(&self) -> u64 {
        let header_bytes = self.headers.iter().fold(0usize, |total, header| {
            total
                .saturating_add(header.key.len())
                .saturating_add(header.value.as_ref().map_or(0, Vec::len))
        });
        let total = self
            .topic
            .len()
            .saturating_add(self.key.as_ref().map_or(0, Vec::len))
            .saturating_add(self.value.as_ref().map_or(0, Vec::len))
            .saturating_add(header_bytes);
        u64::try_from(total).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaMessageSearchField {
    Key,
    Value,
    Headers,
}

impl KafkaMessageSearchField {
    pub const fn all() -> [Self; 3] {
        [Self::Key, Self::Value, Self::Headers]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaMessageQuery {
    pub topic: String,
    pub partitions: Vec<i32>,
    #[serde(default)]
    pub start_offset: Option<i64>,
    #[serde(default)]
    pub end_offset: Option<i64>,
    #[serde(default)]
    pub start_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
    pub max_records: usize,
    pub max_bytes: u64,
    pub max_scan_seconds: u32,
    pub max_concurrent_partitions: usize,
}

impl KafkaMessageQuery {
    /// 创建按 Offset 读取的查询；`end_offset` 是可选的上界。
    pub fn by_offset(
        topic: impl Into<String>,
        partitions: Vec<i32>,
        start_offset: i64,
        end_offset: Option<i64>,
    ) -> Self {
        Self::with_range(
            topic.into(),
            partitions,
            Some(start_offset),
            end_offset,
            None,
            None,
        )
    }

    /// 创建按时间读取的查询；时间范围由 Kafka Consumer 转换为 Partition Offset。
    pub fn by_time(
        topic: impl Into<String>,
        partitions: Vec<i32>,
        start_time: DateTime<Utc>,
        end_time: Option<DateTime<Utc>>,
    ) -> Self {
        Self::with_range(
            topic.into(),
            partitions,
            None,
            None,
            Some(start_time),
            end_time,
        )
    }

    fn with_range(
        topic: String,
        partitions: Vec<i32>,
        start_offset: Option<i64>,
        end_offset: Option<i64>,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            topic,
            partitions,
            start_offset,
            end_offset,
            start_time,
            end_time,
            max_records: DEFAULT_KAFKA_MAX_RECORDS,
            max_bytes: DEFAULT_KAFKA_MAX_BYTES,
            max_scan_seconds: DEFAULT_KAFKA_MAX_SCAN_SECONDS,
            max_concurrent_partitions: DEFAULT_KAFKA_MAX_CONCURRENT_PARTITIONS,
        }
    }

    pub fn with_limits(
        mut self,
        max_records: usize,
        max_bytes: u64,
        max_scan_seconds: u32,
        max_concurrent_partitions: usize,
    ) -> Self {
        self.max_records = max_records;
        self.max_bytes = max_bytes;
        self.max_scan_seconds = max_scan_seconds;
        self.max_concurrent_partitions = max_concurrent_partitions;
        self
    }

    /// 校验读取范围和所有扫描预算，确保基础设施层不会执行无界扫描。
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_topic_name(&self.topic)?;
        validate_partition_ids(&self.partitions)?;
        if self.start_offset.is_none()
            && self.end_offset.is_none()
            && self.start_time.is_none()
            && self.end_time.is_none()
        {
            return Err("消息查询必须指定 Offset 或时间范围".into());
        }
        if self.start_offset.is_some() || self.end_offset.is_some() {
            if self.start_time.is_some() || self.end_time.is_some() {
                return Err("消息查询不能同时使用 Offset 和时间范围".into());
            }
            validate_optional_offset(self.start_offset)?;
            validate_optional_offset(self.end_offset)?;
            if let (Some(start), Some(end)) = (self.start_offset, self.end_offset)
                && start >= end
            {
                return Err("消息 Offset 范围必须满足 start < end".into());
            }
        } else if let (Some(start), Some(end)) = (self.start_time, self.end_time)
            && start >= end
        {
            return Err("消息时间范围必须满足 start < end".into());
        }
        if !(1..=MAX_KAFKA_SCAN_RECORDS).contains(&self.max_records) {
            return Err(format!(
                "最大消息数必须在 1 - {MAX_KAFKA_SCAN_RECORDS} 之间"
            ));
        }
        if !(1..=MAX_KAFKA_SCAN_BYTES).contains(&self.max_bytes) {
            return Err(format!(
                "最大扫描字节数必须在 1 - {MAX_KAFKA_SCAN_BYTES} 之间"
            ));
        }
        if !(1..=MAX_KAFKA_SCAN_SECONDS).contains(&self.max_scan_seconds) {
            return Err(format!(
                "最大扫描时间必须在 1 - {MAX_KAFKA_SCAN_SECONDS} 秒之间"
            ));
        }
        if !(1..=MAX_KAFKA_CONCURRENT_PARTITIONS).contains(&self.max_concurrent_partitions) {
            return Err(format!(
                "最大并发 Partition 数必须在 1 - {MAX_KAFKA_CONCURRENT_PARTITIONS} 之间"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaMessageSearchQuery {
    pub scan: KafkaMessageQuery,
    pub query: String,
    pub fields: Vec<KafkaMessageSearchField>,
}

impl KafkaMessageSearchQuery {
    /// 创建客户端扫描查询，默认在 Key、Value 和 Headers 三处匹配。
    pub fn new(query: impl Into<String>, scan: KafkaMessageQuery) -> Self {
        Self {
            scan,
            query: query.into(),
            fields: KafkaMessageSearchField::all().to_vec(),
        }
    }

    pub fn with_fields(mut self, fields: Vec<KafkaMessageSearchField>) -> Self {
        self.fields = fields;
        self
    }

    /// 校验搜索文本和字段选择，并复用基础读取查询的范围与预算限制。
    pub fn validate(&self) -> Result<(), String> {
        self.scan.validate()?;
        if self.query.is_empty() {
            return Err("消息搜索词不能为空".into());
        }
        validate_protocol_text("消息搜索词", &self.query, MAX_KAFKA_QUERY_TEXT_BYTES)?;
        if self.fields.is_empty() {
            return Err("消息搜索字段不能为空".into());
        }
        if self.fields.len() > KafkaMessageSearchField::all().len() {
            return Err("消息搜索字段数量无效".into());
        }
        let mut fields = HashSet::with_capacity(self.fields.len());
        if self.fields.iter().any(|field| !fields.insert(*field)) {
            return Err("消息搜索字段不能重复".into());
        }
        Ok(())
    }
}

fn validate_partition_ids(partitions: &[i32]) -> Result<(), String> {
    if partitions.is_empty() {
        return Err("消息查询至少需要一个 Partition".into());
    }
    if partitions.len() > MAX_KAFKA_QUERY_PARTITIONS {
        return Err(format!(
            "消息查询 Partition 数量超过 {MAX_KAFKA_QUERY_PARTITIONS} 个上限"
        ));
    }
    let mut seen = HashSet::with_capacity(partitions.len());
    for partition in partitions {
        if *partition < 0 {
            return Err("消息查询 Partition 不能为负数".into());
        }
        if !seen.insert(partition) {
            return Err(format!("消息查询 Partition 重复：{partition}"));
        }
    }
    Ok(())
}
