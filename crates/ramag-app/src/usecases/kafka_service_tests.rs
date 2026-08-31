use std::sync::{Arc, Mutex};

use super::{
    KafkaService, validate_admin_request, validate_cluster_metadata, validate_message_page,
    validate_topics,
};
use async_trait::async_trait;
use ramag_domain::entities::{
    KafkaBroker, KafkaClusterConfig, KafkaClusterMetadata, KafkaConfigEntry, KafkaConfigResource,
    KafkaConfigResourceType, KafkaConfigSource, KafkaConfigUpdateRequest, KafkaMessagePage,
    KafkaMessageRecord, KafkaPartition, KafkaReadOnlyState, KafkaTopic, KafkaTopicCreateRequest,
    KafkaTopicPartitionExpansion,
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

    async fn delete_connection(&self, _id: &ramag_domain::entities::ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn append_history(&self, _record: &ramag_domain::entities::QueryRecord) -> Result<()> {
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

    async fn describe_configs(
        &self,
        _config: &KafkaClusterConfig,
        resource_type: KafkaConfigResourceType,
        resource_name: &str,
    ) -> Result<KafkaConfigResource> {
        self.calls.lock().unwrap().push(format!(
            "describe:{}:{resource_name}",
            resource_type.label()
        ));
        Ok(KafkaConfigResource {
            resource_type,
            resource_name: resource_name.into(),
            entries: vec![KafkaConfigEntry {
                key: "retention.ms".into(),
                value: Some("60000".into()),
                source: KafkaConfigSource::DynamicTopic,
                is_read_only: false,
                is_default: false,
                is_sensitive: false,
            }],
        })
    }

    async fn update_config(
        &self,
        _config: &KafkaClusterConfig,
        request: &KafkaConfigUpdateRequest,
    ) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("update:{}", request.key));
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

#[test]
fn config_service_allows_reads_in_read_only_mode_but_blocks_updates() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service_with_admin(Arc::new(RecordingAdminDriver {
        calls: calls.clone(),
    }));
    let config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
    let resource =
        smol::block_on(service.describe_configs(&config, KafkaConfigResourceType::Topic, "events"))
            .expect("read-only mode should allow config reads");
    assert_eq!(resource.entries[0].key, "retention.ms");
    assert_eq!(&*calls.lock().unwrap(), &["describe:Topic:events"]);

    let request = KafkaConfigUpdateRequest::set(
        KafkaConfigResourceType::Topic,
        "events",
        "retention.ms",
        "120000",
    );
    let result = smol::block_on(service.update_config(&config, &request));
    assert!(matches!(
        result,
        Err(DomainError::Forbidden(message)) if message == ramag_domain::error::READ_ONLY_MESSAGE
    ));
    assert_eq!(&*calls.lock().unwrap(), &["describe:Topic:events"]);
}

#[test]
fn config_service_forwards_valid_update_after_admin_mode_is_enabled() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service_with_admin(Arc::new(RecordingAdminDriver {
        calls: calls.clone(),
    }));
    let mut config = KafkaClusterConfig::new("local", vec!["localhost:9092".into()]);
    config.read_only = KafkaReadOnlyState::ReadWrite;
    let request = KafkaConfigUpdateRequest::set(
        KafkaConfigResourceType::Topic,
        "events",
        "retention.ms",
        "120000",
    );
    assert!(smol::block_on(service.update_config(&config, &request)).is_ok());
    assert_eq!(&*calls.lock().unwrap(), &["update:retention.ms"]);
}
