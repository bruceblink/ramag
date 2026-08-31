//! Kafka 配置、元数据和有界消息读取的应用服务。

use std::sync::Arc;

use ramag_domain::entities::{
    KafkaClusterConfig, KafkaClusterId, KafkaClusterMetadata, KafkaConfigResource,
    KafkaConfigResourceType, KafkaConfigUpdateRequest, KafkaConsumerGroup, KafkaMessagePage,
    KafkaMessageQuery, KafkaMessageSearchQuery, KafkaTopic, KafkaTopicCreateRequest,
    KafkaTopicPartitionExpansion,
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

    /// 读取消费者组、成员和已提交 Offset；该查询只读，不加入任何业务消费者组。
    pub async fn list_consumer_groups(
        &self,
        config: &KafkaClusterConfig,
    ) -> Result<Vec<KafkaConsumerGroup>> {
        validate_config(config)?;
        let started = std::time::Instant::now();
        let result = self.driver.list_consumer_groups(config).await;
        log_runtime_result(
            "kafka_consumer_group_list",
            config,
            started,
            result.as_ref().ok().map(Vec::len),
            result.as_ref().err(),
        );
        result.and_then(validate_consumer_groups)
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

    /// 读取指定 Topic 或 Broker 的配置快照；只读模式仍允许查看配置来源和可见性。
    pub async fn describe_configs(
        &self,
        config: &KafkaClusterConfig,
        resource_type: KafkaConfigResourceType,
        resource_name: &str,
    ) -> Result<KafkaConfigResource> {
        validate_config(config)?;
        resource_type
            .validate_resource_name(resource_name)
            .map_err(DomainError::InvalidConfig)?;
        let started = std::time::Instant::now();
        let result = self
            .admin_driver
            .describe_configs(config, resource_type, resource_name)
            .await
            .and_then(|resource| validate_config_resource(resource, resource_type, resource_name));
        log_config_read_result(
            "kafka_config_describe",
            config,
            resource_type,
            resource_name,
            started,
            &result,
        );
        result
    }

    /// 修改单个动态配置项；调用方必须明确开启管理模式，具体资源来源由驱动再次核验。
    pub async fn update_config(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaConfigUpdateRequest,
    ) -> Result<()> {
        validate_admin_request(config, request.validate())?;
        let started = std::time::Instant::now();
        let result = self.admin_driver.update_config(config, request).await;
        log_config_update_result("kafka_config_update", config, request, started, &result);
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

/// 在应用边界校验消费者组快照，防止驱动实现绕过数量、成员和 Offset 约束。
fn validate_consumer_groups(groups: Vec<KafkaConsumerGroup>) -> Result<Vec<KafkaConsumerGroup>> {
    if groups.len() > ramag_domain::entities::MAX_KAFKA_CONSUMER_GROUPS {
        return Err(DomainError::InvalidConfig(format!(
            "消费者组数量超过 {} 个上限",
            ramag_domain::entities::MAX_KAFKA_CONSUMER_GROUPS
        )));
    }
    let mut ids = std::collections::HashSet::with_capacity(groups.len());
    for group in &groups {
        group.validate().map_err(DomainError::InvalidConfig)?;
        if !ids.insert(group.group_id.as_str()) {
            return Err(DomainError::InvalidConfig(format!(
                "消费者组 ID 重复：{}",
                group.group_id
            )));
        }
    }
    Ok(groups)
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

fn validate_config_resource(
    resource: KafkaConfigResource,
    expected_type: KafkaConfigResourceType,
    expected_name: &str,
) -> Result<KafkaConfigResource> {
    resource.validate().map_err(DomainError::InvalidConfig)?;
    if resource.resource_type != expected_type || resource.resource_name != expected_name {
        return Err(DomainError::InvalidConfig(
            "Kafka 配置资源与请求不一致".into(),
        ));
    }
    Ok(resource)
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

fn log_config_read_result(
    operation: &'static str,
    config: &KafkaClusterConfig,
    resource_type: KafkaConfigResourceType,
    resource_name: &str,
    started: std::time::Instant,
    result: &Result<KafkaConfigResource>,
) {
    tracing::info!(
        operation,
        cluster_id = %config.id,
        resource_type = resource_type.label(),
        resource_name,
        elapsed_ms = started.elapsed().as_millis(),
        entry_count = result.as_ref().map_or(0, |resource| resource.entries.len()),
        success = result.is_ok(),
        "Kafka 配置读取完成"
    );
    if let Err(error) = result {
        tracing::warn!(
            operation,
            cluster_id = %config.id,
            resource_type = resource_type.label(),
            resource_name,
            error = %error,
            "Kafka 配置读取失败"
        );
    }
}

fn log_config_update_result(
    operation: &'static str,
    config: &KafkaClusterConfig,
    request: &KafkaConfigUpdateRequest,
    started: std::time::Instant,
    result: &Result<()>,
) {
    tracing::info!(
        operation,
        cluster_id = %config.id,
        resource_type = request.resource_type.label(),
        resource_name = %request.resource_name,
        config_key = %request.key,
        config_operation = request.operation.label(),
        elapsed_ms = started.elapsed().as_millis(),
        success = result.is_ok(),
        "Kafka 配置修改完成"
    );
    if let Err(error) = result {
        tracing::warn!(
            operation,
            cluster_id = %config.id,
            resource_type = request.resource_type.label(),
            resource_name = %request.resource_name,
            config_key = %request.key,
            config_operation = request.operation.label(),
            error = %error,
            "Kafka 配置修改失败"
        );
    }
}

#[cfg(test)]
#[path = "kafka_service_tests.rs"]
mod tests;
