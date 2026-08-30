//! Kafka 配置、元数据和有界消息读取的应用服务。

use std::sync::Arc;

use ramag_domain::entities::{
    KafkaClusterConfig, KafkaClusterId, KafkaClusterMetadata, KafkaMessagePage, KafkaMessageQuery,
    KafkaMessageSearchQuery, KafkaTopic,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{KafkaDriver, Storage};

pub struct KafkaService {
    driver: Arc<dyn KafkaDriver>,
    storage: Arc<dyn Storage>,
}

impl KafkaService {
    pub fn new(driver: Arc<dyn KafkaDriver>, storage: Arc<dyn Storage>) -> Self {
        Self { driver, storage }
    }

    /// 读取本地保存的 Kafka 集群配置，不包含消息正文或运行时快照。
    pub async fn list_clusters(&self) -> Result<Vec<KafkaClusterConfig>> {
        let result = self.storage.list_kafka_clusters().await;
        log_storage_result("kafka_cluster_list", &result);
        result
    }

    pub async fn get_cluster(&self, id: &KafkaClusterId) -> Result<Option<KafkaClusterConfig>> {
        let result = self.storage.get_kafka_cluster(id).await;
        log_storage_result("kafka_cluster_get", &result);
        result
    }

    /// 保存前执行领域校验；密码仍由 Storage 的加密实现负责保护。
    pub async fn save_cluster(&self, config: &KafkaClusterConfig) -> Result<()> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        let result = self.storage.save_kafka_cluster(config).await;
        log_storage_result("kafka_cluster_save", &result);
        result
    }

    pub async fn delete_cluster(&self, id: &KafkaClusterId) -> Result<()> {
        let result = self.storage.delete_kafka_cluster(id).await;
        log_storage_result("kafka_cluster_delete", &result);
        result
    }

    pub async fn test_connection(&self, config: &KafkaClusterConfig) -> Result<()> {
        validate_config(config)?;
        let started = std::time::Instant::now();
        let result = self.driver.test_connection(config).await;
        tracing::info!(
            operation = "kafka_connection_test",
            cluster_id = %config.id,
            elapsed_ms = started.elapsed().as_millis(),
            success = result.is_ok(),
            "Kafka connection test completed"
        );
        result
    }

    pub async fn cluster_metadata(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<KafkaClusterMetadata> {
        validate_config(config)?;
        let started = std::time::Instant::now();
        let result = self.driver.cluster_metadata(config).await;
        log_runtime_result(
            "kafka_cluster_metadata",
            config,
            started,
            result.as_ref().ok().map(|metadata| metadata.brokers.len()),
            result.as_ref().err(),
        );
        result.and_then(validate_cluster_metadata)
    }

    pub async fn list_topics(&self, config: &KafkaClusterConfig) -> Result<Vec<KafkaTopic>> {
        validate_config(config)?;
        let started = std::time::Instant::now();
        let result = self.driver.list_topics(config).await;
        log_runtime_result(
            "kafka_topic_list",
            config,
            started,
            result.as_ref().ok().map(Vec::len),
            result.as_ref().err(),
        );
        result.and_then(validate_topics)
    }

    pub async fn read_messages(
        &self,
        config: &KafkaClusterConfig,
        query: &KafkaMessageQuery,
    ) -> Result<KafkaMessagePage> {
        validate_config(config)?;
        query.validate().map_err(DomainError::InvalidConfig)?;
        let started = std::time::Instant::now();
        let result = self.driver.read_messages(config, query).await;
        log_message_result(
            "kafka_message_read",
            config,
            query.topic.as_str(),
            started,
            &result,
        );
        result.and_then(validate_message_page)
    }

    pub async fn search_messages(
        &self,
        config: &KafkaClusterConfig,
        query: &KafkaMessageSearchQuery,
    ) -> Result<KafkaMessagePage> {
        validate_config(config)?;
        query.validate().map_err(DomainError::InvalidConfig)?;
        let started = std::time::Instant::now();
        let result = self.driver.search_messages(config, query).await;
        log_message_result(
            "kafka_message_search",
            config,
            query.scan.topic.as_str(),
            started,
            &result,
        );
        result.and_then(validate_message_page)
    }
}

/// 在应用层再次校验驱动返回的页，避免替换基础设施实现时绕过领域资源上限。
fn validate_message_page(page: KafkaMessagePage) -> Result<KafkaMessagePage> {
    page.validate()
        .map(|()| page)
        .map_err(DomainError::InvalidConfig)
}

/// 在应用层校验驱动返回的集群快照，避免不完整或重复的 Broker 污染 UI。
fn validate_cluster_metadata(metadata: KafkaClusterMetadata) -> Result<KafkaClusterMetadata> {
    metadata
        .validate()
        .map(|()| metadata)
        .map_err(DomainError::InvalidConfig)
}

/// 在应用层校验 Topic 列表及其 Partition 快照，保持替换驱动后的边界不变。
fn validate_topics(topics: Vec<KafkaTopic>) -> Result<Vec<KafkaTopic>> {
    if topics.len() > ramag_domain::entities::MAX_KAFKA_TOPICS {
        return Err(DomainError::InvalidConfig(format!(
            "Kafka Topic 数量超过 {} 个上限",
            ramag_domain::entities::MAX_KAFKA_TOPICS
        )));
    }
    let mut names = std::collections::HashSet::with_capacity(topics.len());
    for topic in &topics {
        topic.validate().map_err(DomainError::InvalidConfig)?;
        if !names.insert(topic.name.as_str()) {
            return Err(DomainError::InvalidConfig(format!(
                "Kafka Topic 名称重复：{}",
                topic.name
            )));
        }
    }
    Ok(topics)
}

fn validate_config(config: &KafkaClusterConfig) -> Result<()> {
    config.validate().map_err(DomainError::InvalidConfig)
}

fn log_storage_result<T>(operation: &'static str, result: &Result<T>) {
    if let Err(error) = result {
        tracing::warn!(operation, error = %error, "Kafka local storage operation failed");
    }
}

fn log_runtime_result(
    operation: &'static str,
    config: &KafkaClusterConfig,
    started: std::time::Instant,
    result_count: Option<usize>,
    error: Option<&ramag_domain::error::DomainError>,
) {
    tracing::info!(
        operation,
        cluster_id = %config.id,
        elapsed_ms = started.elapsed().as_millis(),
        result_count,
        success = error.is_none(),
        "Kafka read operation completed"
    );
    if let Some(error) = error {
        tracing::warn!(operation, cluster_id = %config.id, error = %error, "Kafka read operation failed");
    }
}

fn log_message_result(
    operation: &'static str,
    config: &KafkaClusterConfig,
    topic: &str,
    started: std::time::Instant,
    result: &Result<KafkaMessagePage>,
) {
    tracing::info!(
        operation,
        cluster_id = %config.id,
        topic,
        elapsed_ms = started.elapsed().as_millis(),
        result_count = result.as_ref().map_or(0, |page| page.records.len()),
        scanned_records = result.as_ref().map_or(0, |page| page.scanned_records),
        success = result.is_ok(),
        "Kafka message operation completed"
    );
    if let Err(error) = result {
        tracing::warn!(operation, cluster_id = %config.id, topic, error = %error, "Kafka message operation failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_cluster_metadata, validate_message_page, validate_topics};
    use ramag_domain::entities::{
        KafkaBroker, KafkaClusterMetadata, KafkaMessagePage, KafkaMessageRecord, KafkaPartition,
        KafkaTopic,
    };

    #[test]
    fn application_boundary_rejects_invalid_driver_snapshots() {
        let metadata = KafkaClusterMetadata {
            cluster_id: None,
            controller_id: None,
            brokers: Vec::new(),
            kafka_version: None,
        };
        assert!(validate_cluster_metadata(metadata).is_err());

        let topic = KafkaTopic {
            name: "events".into(),
            partitions: vec![KafkaPartition {
                id: 0,
                leader: Some(0),
                replicas: vec![0],
                isr: vec![0],
                low_watermark: Some(0),
                high_watermark: Some(1),
            }],
            internal: false,
        };
        assert!(validate_topics(vec![topic.clone(), topic]).is_err());

        let page = KafkaMessagePage {
            records: vec![KafkaMessageRecord {
                topic: "events".into(),
                partition: 0,
                offset: 0,
                timestamp: None,
                key: None,
                value: Some(b"event".to_vec()),
                headers: Vec::new(),
            }],
            scanned_records: 0,
            scanned_bytes: 0,
            truncated: false,
        };
        assert!(validate_message_page(page).is_err());
    }

    #[test]
    fn application_boundary_accepts_valid_metadata() {
        let metadata = KafkaClusterMetadata {
            cluster_id: Some("cluster".into()),
            controller_id: Some(0),
            brokers: vec![KafkaBroker {
                id: 0,
                host: "broker".into(),
                port: 9092,
                rack: None,
                version: None,
                is_controller: true,
            }],
            kafka_version: None,
        };
        assert!(validate_cluster_metadata(metadata).is_ok());
    }
}
