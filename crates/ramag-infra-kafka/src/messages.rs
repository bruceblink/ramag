use super::*;
use chrono::{DateTime, Utc};
use ramag_domain::entities::{
    KafkaMessageHeader, KafkaMessagePage, KafkaMessageQuery, KafkaMessageRecord,
    KafkaMessageSearchField, KafkaMessageSearchQuery,
};
use rdkafka::message::{Headers as _, Message};
use rdkafka::topic_partition_list::{Offset, TopicPartitionList};
use std::time::{Duration as StdDuration, Instant};

#[derive(Default)]
struct PartitionScan {
    records: Vec<KafkaMessageRecord>,
    scanned_records: usize,
    scanned_bytes: u64,
    truncated: bool,
}

impl RdkafkaDriver {
    /// 在独立消费者上按 Partition 顺序扫描，返回结果和扫描预算统计。
    pub(super) fn scan_messages_blocking(
        &self,
        config: &KafkaClusterConfig,
        query: &KafkaMessageQuery,
        search: Option<&KafkaMessageSearchQuery>,
    ) -> Result<KafkaMessagePage> {
        Self::ensure_build_features(config)?;
        let deadline = Instant::now() + StdDuration::from_secs(u64::from(query.max_scan_seconds));
        let mut page = KafkaMessagePage::empty();

        for &partition in &query.partitions {
            if page.scanned_records >= query.max_records
                || page.scanned_bytes >= query.max_bytes
                || Instant::now() >= deadline
            {
                page.truncated = true;
                break;
            }
            let remaining_records = query.max_records - page.scanned_records;
            let remaining_bytes = query.max_bytes - page.scanned_bytes;
            let scanned = self.scan_partition_blocking(
                config,
                query,
                (
                    partition,
                    remaining_records,
                    remaining_bytes,
                    deadline,
                    search,
                ),
            )?;
            page.scanned_records = page.scanned_records.saturating_add(scanned.scanned_records);
            page.scanned_bytes = page.scanned_bytes.saturating_add(scanned.scanned_bytes);
            page.records.extend(scanned.records);
            if scanned.truncated {
                page.truncated = true;
                break;
            }
        }
        if Instant::now() >= deadline {
            page.truncated = true;
        }
        page.validate().map_err(DomainError::InvalidConfig)?;
        Ok(page)
    }

    fn scan_partition_blocking(
        &self,
        config: &KafkaClusterConfig,
        query: &KafkaMessageQuery,
        scan: (i32, usize, u64, Instant, Option<&KafkaMessageSearchQuery>),
    ) -> Result<PartitionScan> {
        let (partition, max_records, max_bytes, deadline, search) = scan;
        let consumer = self.create_consumer(config)?;
        let (low, high) = consumer
            .fetch_watermarks(&query.topic, partition, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "读取 Kafka Partition 水位"))?;
        let low = low.max(0);
        let high = high.max(low);
        let start = resolve_query_offset(
            &consumer,
            query,
            partition,
            (
                query.start_offset,
                query.start_time,
                low,
                high,
                self.request_timeout,
                "解析 Kafka 消息起始位置",
            ),
        )?
        .unwrap_or(low)
        .clamp(low, high);
        let end = resolve_query_offset(
            &consumer,
            query,
            partition,
            (
                query.end_offset,
                query.end_time,
                low,
                high,
                self.request_timeout,
                "解析 Kafka 消息结束位置",
            ),
        )?
        .unwrap_or(high)
        .clamp(low, high);
        if start >= end {
            return Ok(PartitionScan::default());
        }

        let mut assignment = TopicPartitionList::new();
        assignment
            .add_partition_offset(&query.topic, partition, Offset::Offset(start))
            .map_err(|error| errors::map_kafka_error(error, "分配 Kafka Partition"))?;
        consumer
            .assign(&assignment)
            .map_err(|error| errors::map_kafka_error(error, "分配 Kafka Partition"))?;

        let mut scanned = PartitionScan::default();
        let mut empty_polls = 0u8;
        while Instant::now() < deadline {
            if scanned.scanned_records >= max_records || scanned.scanned_bytes >= max_bytes {
                scanned.truncated = true;
                break;
            }
            let message = match consumer.poll(StdDuration::from_millis(250)) {
                None => {
                    empty_polls = empty_polls.saturating_add(1);
                    if empty_polls >= 8 {
                        break;
                    }
                    continue;
                }
                Some(result) => {
                    result.map_err(|error| errors::map_kafka_error(error, "读取 Kafka 消息"))?
                }
            };
            empty_polls = 0;
            if message.offset() < start {
                continue;
            }
            if message.offset() >= end {
                break;
            }

            let record = record_from_message(&message);
            let record_bytes = record.retained_bytes();
            if scanned.scanned_bytes.saturating_add(record_bytes) > max_bytes {
                scanned.truncated = true;
                break;
            }
            scanned.scanned_records = scanned.scanned_records.saturating_add(1);
            scanned.scanned_bytes = scanned.scanned_bytes.saturating_add(record_bytes);
            if search.is_none_or(|query| message_matches(&record, query)) {
                scanned.records.push(record);
            }
        }
        if Instant::now() >= deadline {
            scanned.truncated = true;
        }
        Ok(scanned)
    }
}

fn resolve_query_offset(
    consumer: &BaseConsumer,
    query: &KafkaMessageQuery,
    partition: i32,
    options: (
        Option<i64>,
        Option<DateTime<Utc>>,
        i64,
        i64,
        Duration,
        &'static str,
    ),
) -> Result<Option<i64>> {
    let (offset, timestamp, low, high, timeout, operation) = options;
    if let Some(offset) = offset {
        return Ok(Some(offset));
    }
    let Some(timestamp) = timestamp else {
        return Ok(None);
    };
    let mut timestamps = TopicPartitionList::new();
    timestamps
        .add_partition_offset(
            &query.topic,
            partition,
            Offset::Offset(timestamp.timestamp_millis().max(0)),
        )
        .map_err(|error| errors::map_kafka_error(error, operation))?;
    let offsets = consumer
        .offsets_for_times(timestamps, timeout)
        .map_err(|error| errors::map_kafka_error(error, operation))?;
    let Some(entry) = offsets.find_partition(&query.topic, partition) else {
        return Ok(Some(high));
    };
    entry
        .error()
        .map_err(|error| errors::map_kafka_error(error, operation))?;
    Ok(match entry.offset() {
        Offset::Offset(value) if value >= 0 => Some(value),
        Offset::Beginning => Some(low),
        Offset::End | Offset::Invalid => Some(high),
        Offset::Stored | Offset::OffsetTail(_) => Some(low),
        Offset::Offset(value) => Some(value.max(low)),
    })
}

fn record_from_message(message: &rdkafka::message::BorrowedMessage<'_>) -> KafkaMessageRecord {
    let headers = message
        .headers()
        .map(|headers| {
            headers
                .iter()
                .map(|header| KafkaMessageHeader {
                    key: header.key.to_owned(),
                    value: header.value.map(ToOwned::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    KafkaMessageRecord {
        topic: message.topic().to_owned(),
        partition: message.partition(),
        offset: message.offset(),
        timestamp: message
            .timestamp()
            .to_millis()
            .and_then(DateTime::<Utc>::from_timestamp_millis),
        key: message.key().map(ToOwned::to_owned),
        value: message.payload().map(ToOwned::to_owned),
        headers,
    }
}

fn message_matches(record: &KafkaMessageRecord, query: &KafkaMessageSearchQuery) -> bool {
    let needle = query.query.to_lowercase();
    query.fields.iter().any(|field| match field {
        KafkaMessageSearchField::Key => record
            .key
            .as_deref()
            .is_some_and(|bytes| text_contains(bytes, &needle)),
        KafkaMessageSearchField::Value => record
            .value
            .as_deref()
            .is_some_and(|bytes| text_contains(bytes, &needle)),
        KafkaMessageSearchField::Headers => record.headers.iter().any(|header| {
            header.key.to_lowercase().contains(&needle)
                || header
                    .value
                    .as_deref()
                    .is_some_and(|bytes| text_contains(bytes, &needle))
        }),
    })
}

fn text_contains(bytes: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(bytes)
        .to_lowercase()
        .contains(needle)
}
