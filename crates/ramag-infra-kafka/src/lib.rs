//! Kafka 基础设施层：使用 `rdkafka` 创建隔离的客户端，并将错误映射为安全领域错误。
mod admin;
#[cfg(feature = "cmake-build")]
mod config;
#[cfg(feature = "cmake-build")]
pub mod errors;
#[cfg(feature = "cmake-build")]
use chrono::{DateTime, Utc};
use ramag_domain::entities::KafkaClusterConfig;
#[cfg(feature = "cmake-build")]
use ramag_domain::entities::{
    KafkaBroker, KafkaClusterMetadata, KafkaMessageHeader, KafkaMessagePage, KafkaMessageQuery,
    KafkaMessageRecord, KafkaMessageSearchField, KafkaMessageSearchQuery, KafkaPartition,
    KafkaTopic, MAX_KAFKA_BROKERS, MAX_KAFKA_PARTITIONS, MAX_KAFKA_TOPICS,
};
use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory, Result};
use ramag_domain::traits::KafkaDriver;
#[cfg(feature = "cmake-build")]
use rdkafka::admin::AdminClient;
#[cfg(feature = "cmake-build")]
use rdkafka::client::DefaultClientContext;
#[cfg(feature = "cmake-build")]
use rdkafka::consumer::{BaseConsumer, Consumer};
#[cfg(feature = "cmake-build")]
use rdkafka::message::{Headers as _, Message};
#[cfg(feature = "cmake-build")]
use rdkafka::topic_partition_list::{Offset, TopicPartitionList};
use std::time::Duration;
#[cfg(feature = "cmake-build")]
use std::time::{Duration as StdDuration, Instant};
use tracing::{debug, info};
pub const DEFAULT_KAFKA_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct RdkafkaDriver {
    request_timeout: Duration,
}

impl RdkafkaDriver {
    /// 创建使用固定默认请求预算的 Kafka 驱动。
    pub fn new() -> Self {
        Self {
            request_timeout: DEFAULT_KAFKA_REQUEST_TIMEOUT,
        }
    }

    /// 创建可指定请求预算的驱动，供集成测试和调用方控制连接等待时间。
    pub fn with_request_timeout(request_timeout: Duration) -> Result<Self> {
        validate_request_timeout(request_timeout)?;
        Ok(Self { request_timeout })
    }

    /// 在进入 native 客户端前检查构建能力，避免把不支持的安全配置交给底层库。
    fn ensure_build_features(config: &KafkaClusterConfig) -> Result<()> {
        if config.uses_sasl() && !cfg!(feature = "kafka-sasl") {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Unsupported,
                "创建连接客户端",
                "当前构建未启用 Kafka SASL 支持",
            )));
        }
        if config.uses_tls() && !cfg!(feature = "kafka-tls") {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Tls,
                "创建连接客户端",
                "当前构建未启用 Kafka TLS 支持",
            )));
        }
        Ok(())
    }

    #[cfg(not(feature = "cmake-build"))]
    /// 默认构建不拉入 librdkafka；启用 `cmake-build` 后才提供真实连接能力。
    fn test_connection_blocking(&self, config: &KafkaClusterConfig) -> Result<()> {
        let _ = self.request_timeout;
        Self::ensure_build_features(config)?;
        Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Unsupported,
            "创建连接客户端",
            "当前构建未启用 Kafka native 客户端；请启用 cmake-build feature",
        )))
    }

    #[cfg(not(feature = "cmake-build"))]
    fn cluster_metadata_blocking(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<ramag_domain::entities::KafkaClusterMetadata> {
        let _ = self.request_timeout;
        Self::ensure_build_features(config)?;
        Err(native_client_unavailable("读取 Kafka 集群元数据"))
    }

    #[cfg(not(feature = "cmake-build"))]
    fn list_topics_blocking(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<Vec<ramag_domain::entities::KafkaTopic>> {
        let _ = self.request_timeout;
        Self::ensure_build_features(config)?;
        Err(native_client_unavailable("读取 Kafka Topic 元数据"))
    }

    #[cfg(not(feature = "cmake-build"))]
    fn scan_messages_blocking(
        &self,
        config: &KafkaClusterConfig,
        _query: &ramag_domain::entities::KafkaMessageQuery,
        _search: Option<&ramag_domain::entities::KafkaMessageSearchQuery>,
    ) -> Result<ramag_domain::entities::KafkaMessagePage> {
        let _ = self.request_timeout;
        Self::ensure_build_features(config)?;
        Err(native_client_unavailable("读取 Kafka 消息"))
    }

    #[cfg(feature = "cmake-build")]
    /// 在阻塞线程中创建 Admin Client 并拉取集群元数据，以验证连接配置和网络可达性。
    fn test_connection_blocking(&self, config: &KafkaClusterConfig) -> Result<()> {
        Self::ensure_build_features(config)?;
        let client_config = config::build_client_config(config, self.request_timeout)?;
        let admin: AdminClient<DefaultClientContext> = client_config
            .create()
            .map_err(|error| errors::map_kafka_error(error, "创建连接客户端"))?;
        admin
            .inner()
            .fetch_metadata(None, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "测试 Kafka 连接"))?;
        Ok(())
    }

    #[cfg(feature = "cmake-build")]
    fn create_consumer(&self, config: &KafkaClusterConfig) -> Result<BaseConsumer> {
        Self::ensure_build_features(config)?;
        let mut client_config = config::build_client_config(config, self.request_timeout)?;
        // 手动分配 Partition 仍需 group.id；使用临时 UUID，且客户端配置关闭自动提交，
        // 避免浏览任务加入或推进用户已有的业务消费组。
        client_config.set(
            "group.id",
            format!("ramag-kafka-browser-{}", uuid::Uuid::new_v4()),
        );
        client_config
            .create()
            .map_err(|error| errors::map_kafka_error(error, "创建 Kafka 读取客户端"))
    }

    #[cfg(feature = "cmake-build")]
    /// 读取当前集群的元数据；消费者没有 group.id，因此不会加入业务消费组。
    fn cluster_metadata_blocking(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<KafkaClusterMetadata> {
        let consumer = self.create_consumer(config)?;
        let metadata = consumer
            .fetch_metadata(None, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "读取 Kafka 集群元数据"))?;
        let brokers = metadata
            .brokers()
            .iter()
            .map(|broker| {
                let port = u16::try_from(broker.port()).map_err(|_| {
                    DomainError::InvalidConfig(format!(
                        "Kafka Broker 端口超出 1 - 65535 范围：{}",
                        broker.port()
                    ))
                })?;
                Ok(KafkaBroker {
                    id: broker.id(),
                    host: broker.host().to_owned(),
                    port,
                    rack: None,
                    version: None,
                    is_controller: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if brokers.len() > MAX_KAFKA_BROKERS {
            return Err(DomainError::InvalidConfig(format!(
                "Kafka Broker 数量超过 {MAX_KAFKA_BROKERS} 个上限"
            )));
        }
        let result = KafkaClusterMetadata {
            cluster_id: consumer.client().fetch_cluster_id(self.request_timeout),
            // librdkafka 的 metadata wrapper 不暴露 Controller ID，不能根据 Broker 顺序猜测。
            controller_id: None,
            brokers,
            kafka_version: None,
        };
        result.validate().map_err(DomainError::InvalidConfig)?;
        Ok(result)
    }

    #[cfg(feature = "cmake-build")]
    /// 读取 Topic、Partition 和水位；水位查询仍使用同一个无消费组读取客户端。
    fn list_topics_blocking(&self, config: &KafkaClusterConfig) -> Result<Vec<KafkaTopic>> {
        let consumer = self.create_consumer(config)?;
        let metadata = consumer
            .fetch_metadata(None, self.request_timeout)
            .map_err(|error| errors::map_kafka_error(error, "读取 Kafka Topic 元数据"))?;
        if metadata.topics().len() > MAX_KAFKA_TOPICS {
            return Err(DomainError::InvalidConfig(format!(
                "Kafka Topic 数量超过 {MAX_KAFKA_TOPICS} 个上限"
            )));
        }

        let mut topics = Vec::with_capacity(metadata.topics().len());
        for topic_metadata in metadata.topics() {
            if let Some(error) = topic_metadata.error() {
                return Err(errors::map_kafka_error(
                    rdkafka::error::KafkaError::MetadataFetch(error.into()),
                    "读取 Kafka Topic 元数据",
                ));
            }
            if topic_metadata.partitions().len() > MAX_KAFKA_PARTITIONS {
                return Err(DomainError::InvalidConfig(format!(
                    "Topic Partition 数量超过 {MAX_KAFKA_PARTITIONS} 个上限：{}",
                    topic_metadata.name()
                )));
            }
            let name = topic_metadata.name().to_owned();
            let mut partitions = Vec::with_capacity(topic_metadata.partitions().len());
            for partition_metadata in topic_metadata.partitions() {
                let (low, high) = consumer
                    .fetch_watermarks(&name, partition_metadata.id(), self.request_timeout)
                    .map_err(|error| errors::map_kafka_error(error, "读取 Kafka Partition 水位"))?;
                partitions.push(KafkaPartition {
                    id: partition_metadata.id(),
                    leader: (partition_metadata.leader() >= 0)
                        .then_some(partition_metadata.leader()),
                    replicas: partition_metadata.replicas().to_vec(),
                    isr: partition_metadata.isr().to_vec(),
                    low_watermark: (low >= 0).then_some(low),
                    high_watermark: (high >= 0).then_some(high),
                });
            }
            let topic = KafkaTopic {
                internal: name.starts_with("__"),
                name,
                partitions,
            };
            topic.validate().map_err(DomainError::InvalidConfig)?;
            topics.push(topic);
        }
        Ok(topics)
    }

    #[cfg(feature = "cmake-build")]
    /// 在独立消费者上按 Partition 顺序扫描，返回结果和扫描预算统计。
    fn scan_messages_blocking(
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

    #[cfg(feature = "cmake-build")]
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

#[cfg(feature = "cmake-build")]
#[derive(Default)]
struct PartitionScan {
    records: Vec<KafkaMessageRecord>,
    scanned_records: usize,
    scanned_bytes: u64,
    truncated: bool,
}

#[cfg(feature = "cmake-build")]
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

#[cfg(feature = "cmake-build")]
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

#[cfg(feature = "cmake-build")]
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

#[cfg(feature = "cmake-build")]
fn text_contains(bytes: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(bytes)
        .to_lowercase()
        .contains(needle)
}

/// 校验对外暴露的请求时间预算，防止亚毫秒值转换后变成无效的零超时。
fn validate_request_timeout(request_timeout: Duration) -> Result<()> {
    if request_timeout.as_millis() == 0 {
        return Err(DomainError::InvalidConfig(
            "Kafka 请求超时必须至少为 1 毫秒".into(),
        ));
    }
    Ok(())
}

#[cfg(not(feature = "cmake-build"))]
fn native_client_unavailable(operation: &'static str) -> DomainError {
    DomainError::Kafka(KafkaError::new(
        KafkaErrorCategory::Unsupported,
        operation,
        "当前构建未启用 Kafka native 客户端；请启用 cmake-build feature",
    ))
}

impl Default for RdkafkaDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl KafkaDriver for RdkafkaDriver {
    async fn test_connection(&self, config: &KafkaClusterConfig) -> Result<()> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        debug!(
            operation = "kafka_test_connection",
            "starting Kafka connection test"
        );
        smol::unblock(move || driver.test_connection_blocking(&config)).await?;
        info!(
            operation = "kafka_test_connection",
            "Kafka connection test passed"
        );
        Ok(())
    }

    async fn cluster_metadata(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<ramag_domain::entities::KafkaClusterMetadata> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        smol::unblock(move || driver.cluster_metadata_blocking(&config)).await
    }

    async fn list_topics(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<Vec<ramag_domain::entities::KafkaTopic>> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        smol::unblock(move || driver.list_topics_blocking(&config)).await
    }

    async fn read_messages(
        &self,
        config: &KafkaClusterConfig,
        query: &ramag_domain::entities::KafkaMessageQuery,
    ) -> Result<ramag_domain::entities::KafkaMessagePage> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        query.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let query = query.clone();
        smol::unblock(move || driver.scan_messages_blocking(&config, &query, None)).await
    }

    async fn search_messages(
        &self,
        config: &KafkaClusterConfig,
        query: &ramag_domain::entities::KafkaMessageSearchQuery,
    ) -> Result<ramag_domain::entities::KafkaMessagePage> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        query.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let query = query.clone();
        smol::unblock(move || driver.scan_messages_blocking(&config, &query.scan, Some(&query)))
            .await
    }
}
#[cfg(test)]
mod tests;
