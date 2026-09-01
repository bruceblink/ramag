use super::super::RdkafkaDriver;
use super::validate_admin_config;
#[cfg(feature = "cmake-build")]
use super::{
    acl_operations, create_admin_client, describe_config_resource, update_config_incrementally,
    validate_topic_admin_result,
};
use ramag_domain::entities::{
    KafkaAcl, KafkaAclFilter, KafkaClusterConfig, KafkaTopicCreateRequest,
    KafkaTopicPartitionExpansion, validate_kafka_managed_topic_name,
};
#[cfg(feature = "cmake-build")]
use ramag_domain::entities::{
    KafkaConfigResource, KafkaConfigResourceType, KafkaConfigUpdateRequest,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::KafkaAdminDriver;
#[cfg(feature = "cmake-build")]
use ramag_domain::traits::KafkaDriver;
#[cfg(feature = "cmake-build")]
use rdkafka::admin::{AdminOptions, NewPartitions, NewTopic, TopicReplication};

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

    async fn describe_configs(
        &self,
        config: &KafkaClusterConfig,
        resource_type: KafkaConfigResourceType,
        resource_name: &str,
    ) -> Result<KafkaConfigResource> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        resource_type
            .validate_resource_name(resource_name)
            .map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let resource_name = resource_name.to_owned();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            describe_config_resource(
                &admin,
                resource_type,
                &resource_name,
                driver.request_timeout,
            )
        })
        .await
    }

    async fn update_config(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaConfigUpdateRequest,
    ) -> Result<()> {
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let request = request.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            let current = describe_config_resource(
                &admin,
                request.resource_type,
                &request.resource_name,
                driver.request_timeout,
            )?;
            let entry = current.entry(&request.key).ok_or_else(|| {
                DomainError::InvalidConfig("Kafka 配置键不存在，未执行修改".into())
            })?;
            if !entry.can_modify(request.resource_type) {
                return Err(DomainError::Forbidden("该 Kafka 配置项不可修改".into()));
            }
            update_config_incrementally(&admin, &request, driver.request_timeout)
        })
        .await
    }

    async fn list_acls(
        &self,
        config: &KafkaClusterConfig,
        filter: &KafkaAclFilter,
    ) -> Result<Vec<KafkaAcl>> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        filter.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let filter = filter.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            acl_operations::describe_acls_native(&admin, &filter, driver.request_timeout)
        })
        .await
    }

    async fn create_acl(&self, config: &KafkaClusterConfig, acl: &KafkaAcl) -> Result<()> {
        validate_admin_config(config)?;
        acl.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let acl = acl.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            acl_operations::create_acl_native(&admin, &acl, driver.request_timeout)
        })
        .await
    }

    async fn delete_acl(&self, config: &KafkaClusterConfig, acl: &KafkaAcl) -> Result<()> {
        validate_admin_config(config)?;
        acl.validate().map_err(DomainError::InvalidConfig)?;
        let driver = *self;
        let config = config.clone();
        let acl = acl.clone();
        smol::unblock(move || {
            let admin = create_admin_client(&driver, &config)?;
            acl_operations::delete_acl_native(&admin, &acl, driver.request_timeout)
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
        Err(super::super::native_client_unavailable(
            "创建 Kafka 管理客户端",
        ))
    }

    async fn create_topic(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicCreateRequest,
    ) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable("创建 Kafka Topic"))
    }

    async fn delete_topic(&self, config: &KafkaClusterConfig, topic: &str) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        validate_kafka_managed_topic_name(topic).map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable("删除 Kafka Topic"))
    }

    async fn increase_topic_partitions(
        &self,
        config: &KafkaClusterConfig,
        request: &KafkaTopicPartitionExpansion,
    ) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        request.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable(
            "增加 Kafka Topic Partition",
        ))
    }

    async fn list_acls(
        &self,
        config: &KafkaClusterConfig,
        filter: &KafkaAclFilter,
    ) -> Result<Vec<KafkaAcl>> {
        let _ = self.request_timeout;
        config.validate().map_err(DomainError::InvalidConfig)?;
        filter.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable("读取 Kafka ACL"))
    }

    async fn create_acl(&self, config: &KafkaClusterConfig, acl: &KafkaAcl) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        acl.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable("创建 Kafka ACL"))
    }

    async fn delete_acl(&self, config: &KafkaClusterConfig, acl: &KafkaAcl) -> Result<()> {
        let _ = self.request_timeout;
        validate_admin_config(config)?;
        acl.validate().map_err(DomainError::InvalidConfig)?;
        Err(super::super::native_client_unavailable("删除 Kafka ACL"))
    }
}
