//! Kafka 配置、元数据和有界消息读取的应用服务。

use std::sync::Arc;

use ramag_domain::entities::{
    KafkaClusterConfig, KafkaClusterId, KafkaClusterMetadata, KafkaMessagePage, KafkaMessageQuery,
    KafkaMessageSearchQuery, KafkaTopic, KafkaTopicCreateRequest, KafkaTopicPartitionExpansion,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::{KafkaAdminDriver, KafkaDriver, Storage};

pub struct KafkaService {
    driver: Arc<dyn KafkaDriver>,
    admin_driver: Arc<dyn KafkaAdminDriver>,
    storage: Arc<dyn Storage>,
}

impl KafkaService {
    pub fn new(driver: Arc<dyn KafkaDriver>, storage: Arc<dyn Storage>) -> Self {
        Self {
            driver,
            admin_driver: Arc::new(UnsupportedKafkaAdminDriver),
            storage,
        }
    }

    pub fn with_admin_driver(mut self, admin_driver: Arc<dyn KafkaAdminDriver>) -> Self {
        self.admin_driver = admin_driver;
        self
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

    pub async fn create_topic(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicCreateRequest,
    ) -> Result<()> {
        validate_admin_request(config, request.validate())?;
        let started = std::time::Instant::now();
        let result = self.admin_driver.create_topic(config, request).await;
        log_admin_result(
            "kafka_topic_create",
            config,
            &request.name,
            started,
            &result,
        );
        result
    }

    pub async fn delete_topic(&self, config: &KafkaClusterConfig, topic: &str) -> Result<()> {
        validate_config(config)?;
        ramag_domain::entities::validate_kafka_managed_topic_name(topic)
            .map_err(DomainError::InvalidConfig)?;
        ensure_admin_enabled(config)?;
        let started = std::time::Instant::now();
        let result = self.admin_driver.delete_topic(config, topic).await;
        log_admin_result("kafka_topic_delete", config, topic, started, &result);
        result
    }

    pub async fn increase_topic_partitions(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicPartitionExpansion,
    ) -> Result<()> {
        validate_admin_request(config, request.validate())?;
        let started = std::time::Instant::now();
        let result = self
            .admin_driver
            .increase_topic_partitions(config, request)
            .await;
        log_admin_result(
            "kafka_topic_partition_increase",
            config,
            &request.name,
            started,
            &result,
        );
        result
    }
}

struct UnsupportedKafkaAdminDriver;

impl KafkaAdminDriver for UnsupportedKafkaAdminDriver {}

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

fn validate_admin_request(
    config: &KafkaClusterConfig,
    request: std::result::Result<(), String>,
) -> Result<()> {
    validate_config(config)?;
    request.map_err(DomainError::InvalidConfig)?;
    ensure_admin_enabled(config)
}

fn ensure_admin_enabled(config: &KafkaClusterConfig) -> Result<()> {
    if config.read_only.allows_admin() {
        Ok(())
    } else {
        Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()))
    }
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

fn log_admin_result(
    operation: &'static str,
    config: &KafkaClusterConfig,
    topic: &str,
    started: std::time::Instant,
    result: &Result<()>,
) {
    tracing::info!(
        operation,
        cluster_id = %config.id,
        topic,
        elapsed_ms = started.elapsed().as_millis(),
        success = result.is_ok(),
        "Kafka 管理操作完成"
    );
    if let Err(error) = result {
        tracing::warn!(operation, cluster_id = %config.id, topic, error = %error, "Kafka 管理操作失败");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        KafkaService, validate_admin_request, validate_cluster_metadata, validate_message_page,
        validate_topics,
    };
    use async_trait::async_trait;
    use ramag_domain::entities::{
        KafkaBroker, KafkaClusterConfig, KafkaClusterMetadata, KafkaMessagePage,
        KafkaMessageRecord, KafkaPartition, KafkaReadOnlyState, KafkaTopic,
        KafkaTopicCreateRequest, KafkaTopicPartitionExpansion,
    };
    use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory, Result};
    use ramag_domain::traits::{KafkaAdminDriver, KafkaDriver, Storage};

    struct NoopKafkaDriver;

    #[async_trait]
    impl KafkaDriver for NoopKafkaDriver {
        async fn test_connection(&self, _config: &KafkaClusterConfig) -> Result<()> {
            Ok(())
        }
    }

    struct NoopStorage;

    #[async_trait]
    impl Storage for NoopStorage {
        async fn list_connections(&self) -> Result<Vec<ramag_domain::entities::ConnectionConfig>> {
            Ok(Vec::new())
        }

        async fn get_connection(
            &self,
            _id: &ramag_domain::entities::ConnectionId,
        ) -> Result<Option<ramag_domain::entities::ConnectionConfig>> {
            Ok(None)
        }

        async fn save_connection(
            &self,
            _config: &ramag_domain::entities::ConnectionConfig,
        ) -> Result<()> {
            Ok(())
        }

        async fn delete_connection(
            &self,
            _id: &ramag_domain::entities::ConnectionId,
        ) -> Result<()> {
            Ok(())
        }

        async fn append_history(
            &self,
            _record: &ramag_domain::entities::QueryRecord,
        ) -> Result<()> {
            Ok(())
        }

        async fn list_history(
            &self,
            _connection_id: Option<&ramag_domain::entities::ConnectionId>,
            _limit: usize,
        ) -> Result<Vec<ramag_domain::entities::QueryRecord>> {
            Ok(Vec::new())
        }

        async fn delete_history(&self, _id: &ramag_domain::entities::QueryRecordId) -> Result<()> {
            Ok(())
        }

        async fn clear_history(
            &self,
            _connection_id: Option<&ramag_domain::entities::ConnectionId>,
        ) -> Result<()> {
            Ok(())
        }

        async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
            Ok(())
        }
    }

    struct RecordingAdminDriver {
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl KafkaAdminDriver for RecordingAdminDriver {
        async fn create_topic(
            &self,
            _config: &KafkaClusterConfig,
            request: &KafkaTopicCreateRequest,
        ) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create:{}", request.name));
            Ok(())
        }

        async fn delete_topic(&self, _config: &KafkaClusterConfig, topic: &str) -> Result<()> {
            self.calls.lock().unwrap().push(format!("delete:{topic}"));
            Ok(())
        }

        async fn increase_topic_partitions(
            &self,
            _config: &KafkaClusterConfig,
            request: &KafkaTopicPartitionExpansion,
        ) -> Result<()> {
            self.calls.lock().unwrap().push(format!(
                "expand:{}:{}",
                request.name, request.total_partitions
            ));
            Ok(())
        }
    }

    struct FailingAdminDriver;

    #[async_trait]
    impl KafkaAdminDriver for FailingAdminDriver {
        async fn create_topic(
            &self,
            _config: &KafkaClusterConfig,
            _request: &KafkaTopicCreateRequest,
        ) -> Result<()> {
            Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::PermissionDenied,
                "create_topic",
                "Broker 拒绝 Topic 创建",
            )))
        }
    }

    fn service_with_admin(admin: Arc<dyn KafkaAdminDriver>) -> KafkaService {
        KafkaService::new(Arc::new(NoopKafkaDriver), Arc::new(NoopStorage)).with_admin_driver(admin)
    }

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

    #[test]
    fn topic_admin_boundary_requires_explicit_read_write_mode() {
        let mut config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
        let request = KafkaTopicCreateRequest::new("events", 1, 1);
        let result = validate_admin_request(&config, request.validate());
        assert!(matches!(
            result,
            Err(DomainError::Forbidden(message)) if message == ramag_domain::error::READ_ONLY_MESSAGE
        ));

        config.read_only = KafkaReadOnlyState::ReadWrite;
        assert!(validate_admin_request(&config, request.validate()).is_ok());
    }

    #[test]
    fn topic_admin_service_blocks_all_writes_until_mode_is_enabled() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = service_with_admin(Arc::new(RecordingAdminDriver {
            calls: calls.clone(),
        }));
        let config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
        let create = KafkaTopicCreateRequest::new("events", 1, 1);
        let expand = KafkaTopicPartitionExpansion::new("events", 2);

        assert!(matches!(
            smol::block_on(service.create_topic(&config, &create)),
            Err(DomainError::Forbidden(message)) if message == ramag_domain::error::READ_ONLY_MESSAGE
        ));
        assert!(matches!(
            smol::block_on(service.delete_topic(&config, "events")),
            Err(DomainError::Forbidden(message)) if message == ramag_domain::error::READ_ONLY_MESSAGE
        ));
        assert!(matches!(
            smol::block_on(service.increase_topic_partitions(&config, &expand)),
            Err(DomainError::Forbidden(message)) if message == ramag_domain::error::READ_ONLY_MESSAGE
        ));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn topic_admin_service_forwards_valid_operations_and_preserves_driver_errors() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = service_with_admin(Arc::new(RecordingAdminDriver {
            calls: calls.clone(),
        }));
        let mut config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
        config.read_only = KafkaReadOnlyState::ReadWrite;
        let create = KafkaTopicCreateRequest::new("events", 1, 1);
        let expand = KafkaTopicPartitionExpansion::new("events", 2);

        assert!(smol::block_on(service.create_topic(&config, &create)).is_ok());
        assert!(smol::block_on(service.delete_topic(&config, "events")).is_ok());
        assert!(smol::block_on(service.increase_topic_partitions(&config, &expand)).is_ok());
        assert_eq!(
            &*calls.lock().unwrap(),
            &["create:events", "delete:events", "expand:events:2"]
        );

        let failing = service_with_admin(Arc::new(FailingAdminDriver));
        let result = smol::block_on(failing.create_topic(&config, &create));
        assert!(matches!(
            result,
            Err(DomainError::Kafka(error))
                if error.category == KafkaErrorCategory::PermissionDenied
                    && error.operation == "create_topic"
        ));
    }
}
