//! Kafka Admin API 适配；所有变更操作都在独立驱动端口上执行。

use ramag_domain::entities::{
    KafkaClusterConfig, KafkaTopicCreateRequest, KafkaTopicPartitionExpansion,
    validate_kafka_managed_topic_name,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
#[cfg(feature = "cmake-build")]
use ramag_domain::error::{KafkaError, KafkaErrorCategory};
use ramag_domain::traits::KafkaAdminDriver;
#[cfg(feature = "cmake-build")]
use ramag_domain::traits::KafkaDriver;

use super::RdkafkaDriver;

#[cfg(feature = "cmake-build")]
use rdkafka::admin::{AdminClient, AdminOptions, NewPartitions, NewTopic, TopicReplication};
#[cfg(feature = "cmake-build")]
use rdkafka::client::DefaultClientContext;

#[cfg(feature = "cmake-build")]
fn create_admin_client(
    driver: &RdkafkaDriver,
    config: &KafkaClusterConfig,
) -> Result<AdminClient<DefaultClientContext>> {
    RdkafkaDriver::ensure_build_features(config)?;
    let client_config = super::config::build_client_config(config, driver.request_timeout)?;
    client_config
        .create()
        .map_err(|error| super::errors::map_kafka_error(error, "创建 Kafka 管理客户端"))
}

fn validate_admin_config(config: &KafkaClusterConfig) -> Result<()> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if !config.read_only.allows_admin() {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    Ok(())
}

#[cfg(feature = "cmake-build")]
fn validate_topic_admin_result(
    result: std::result::Result<Vec<rdkafka::admin::TopicResult>, rdkafka::error::KafkaError>,
    operation: &'static str,
) -> Result<()> {
    let results = result.map_err(|error| super::errors::map_kafka_error(error, operation))?;
    let Some(topic_result) = results.into_iter().next() else {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Unknown,
            operation,
            format!("Kafka 操作失败：{operation}（Broker 未返回结果）"),
        )));
    };
    match topic_result {
        Ok(_) => Ok(()),
        Err((_topic, code)) => Err(super::errors::map_kafka_error(
            rdkafka::error::KafkaError::AdminOp(code),
            operation,
        )),
    }
}

#[cfg(feature = "cmake-build")]
#[async_trait::async_trait]
impl KafkaAdminDriver for RdkafkaDriver {
    async fn test_admin_connection(&self, config: &KafkaClusterConfig) -> Result<()> {
        <Self as KafkaDriver>::test_connection(self, config).await
    }

    async fn create_topic(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicCreateRequest,
    ) -> Result<()> {
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let request = request.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            let topic = NewTopic::new(
                &request.name,
                i32::try_from(request.partitions).map_err(|_| {
                    DomainError::InvalidConfig("Topic Partition 数量超出 Kafka 客户端范围".into())
                })?,
                TopicReplication::Fixed(i32::try_from(request.replication_factor).map_err(
                    |_| DomainError::InvalidConfig("Topic 副本因子超出 Kafka 客户端范围".into()),
                )?),
            );
            let options = AdminOptions::new().request_timeout(Some(driver.request_timeout));
            let result = smol::block_on(admin.create_topics([&topic], &options));
            validate_topic_admin_result(result, "创建 Kafka Topic")
        })
        .await
    }

    async fn delete_topic(&self, config: &KafkaClusterConfig, topic: &str) -> Result<()> {
        validate_admin_config(config)?;
        validate_kafka_managed_topic_name(topic).map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let topic = topic.to_owned();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            let options = AdminOptions::new().request_timeout(Some(driver.request_timeout));
            let result = smol::block_on(admin.delete_topics(&[topic.as_str()], &options));
            validate_topic_admin_result(result, "删除 Kafka Topic")
        })
        .await
    }

    async fn increase_topic_partitions(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicPartitionExpansion,
    ) -> Result<()> {
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let request = request.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            let partitions = NewPartitions::new(&request.name, request.total_partitions);
            let options = AdminOptions::new().request_timeout(Some(driver.request_timeout));
            let result = smol::block_on(admin.create_partitions([&partitions], &options));
            validate_topic_admin_result(result, "增加 Kafka Topic Partition")
        })
        .await
    }
}

#[cfg(not(feature = "cmake-build"))]
#[async_trait::async_trait]
impl KafkaAdminDriver for RdkafkaDriver {
    async fn test_admin_connection(&self, config: &KafkaClusterConfig) -> Result<()> {
        validate_admin_config(config)?;
        let _ = self.request_timeout;
        Err(super::native_client_unavailable("创建 Kafka 管理客户端"))
    }

    async fn create_topic(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicCreateRequest,
    ) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::native_client_unavailable("创建 Kafka Topic"))
    }

    async fn delete_topic(&self, config: &KafkaClusterConfig, topic: &str) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        validate_kafka_managed_topic_name(topic).map_err(DomainError::InvalidConfig)?;
        Err(super::native_client_unavailable("删除 Kafka Topic"))
    }

    async fn increase_topic_partitions(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicPartitionExpansion,
    ) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::native_client_unavailable(
            "增加 Kafka Topic Partition",
        ))
    }
}
