//! Kafka Admin API 适配；所有变更操作都在独立驱动端口上执行。

use ramag_domain::entities::{
    KafkaClusterConfig, KafkaTopicCreateRequest, KafkaTopicPartitionExpansion,
    validate_kafka_managed_topic_name,
};
#[cfg(feature = "cmake-build")]
use ramag_domain::entities::{
    KafkaConfigEntry, KafkaConfigResource, KafkaConfigResourceType, KafkaConfigSource,
    KafkaConfigUpdateOperation, KafkaConfigUpdateRequest,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
#[cfg(feature = "cmake-build")]
use ramag_domain::error::{KafkaError, KafkaErrorCategory};
use ramag_domain::traits::KafkaAdminDriver;
#[cfg(feature = "cmake-build")]
use ramag_domain::traits::KafkaDriver;

use super::RdkafkaDriver;

#[cfg(feature = "cmake-build")]
use rdkafka::admin::{
    AdminClient, AdminOptions, ConfigResource, ConfigSource, NewPartitions, NewTopic,
    ResourceSpecifier, TopicReplication,
};
#[cfg(feature = "cmake-build")]
use rdkafka::client::DefaultClientContext;
#[cfg(feature = "cmake-build")]
use rdkafka::error::IsError;
#[cfg(feature = "cmake-build")]
use std::ffi::{CString, c_char};
#[cfg(feature = "cmake-build")]
use std::ptr;
#[cfg(feature = "cmake-build")]
use std::time::{Duration, Instant};

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
fn config_resource_specifier<'a>(
    resource_type: KafkaConfigResourceType,
    resource_name: &'a str,
) -> Result<ResourceSpecifier<'a>> {
    resource_type
        .validate_resource_name(resource_name)
        .map_err(DomainError::InvalidConfig)?;
    Ok(match resource_type {
        KafkaConfigResourceType::Topic => ResourceSpecifier::Topic(resource_name),
        KafkaConfigResourceType::Broker => ResourceSpecifier::Broker(
            resource_name
                .parse::<i32>()
                .map_err(|_| DomainError::InvalidConfig("Broker ID 无效".into()))?,
        ),
    })
}

#[cfg(feature = "cmake-build")]
fn map_config_source(source: ConfigSource) -> KafkaConfigSource {
    match source {
        ConfigSource::Unknown => KafkaConfigSource::Unknown,
        ConfigSource::DynamicTopic => KafkaConfigSource::DynamicTopic,
        ConfigSource::DynamicBroker => KafkaConfigSource::DynamicBroker,
        ConfigSource::DynamicDefaultBroker => KafkaConfigSource::DynamicDefaultBroker,
        ConfigSource::StaticBroker => KafkaConfigSource::StaticBroker,
        ConfigSource::Default => KafkaConfigSource::Default,
    }
}

#[cfg(feature = "cmake-build")]
fn map_config_resource(resource: ConfigResource) -> Result<KafkaConfigResource> {
    let (resource_type, resource_name) = match resource.specifier {
        rdkafka::admin::OwnedResourceSpecifier::Topic(name) => {
            (KafkaConfigResourceType::Topic, name)
        }
        rdkafka::admin::OwnedResourceSpecifier::Broker(id) => {
            (KafkaConfigResourceType::Broker, id.to_string())
        }
        rdkafka::admin::OwnedResourceSpecifier::Group(_) => {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Protocol,
                "读取 Kafka 配置",
                "Kafka 返回了不支持的配置资源类型",
            )));
        }
    };
    let entries = resource
        .entries
        .into_iter()
        .map(|entry| KafkaConfigEntry {
            key: entry.name,
            value: if entry.is_sensitive {
                None
            } else {
                entry.value
            },
            source: map_config_source(entry.source),
            is_read_only: entry.is_read_only,
            is_default: entry.is_default,
            is_sensitive: entry.is_sensitive,
        })
        .collect();
    let resource = KafkaConfigResource {
        resource_type,
        resource_name,
        entries,
    };
    resource.validate().map_err(DomainError::InvalidConfig)?;
    Ok(resource)
}

#[cfg(feature = "cmake-build")]
fn describe_config_resource(
    admin: &AdminClient<DefaultClientContext>,
    resource_type: KafkaConfigResourceType,
    resource_name: &str,
    request_timeout: Duration,
) -> Result<KafkaConfigResource> {
    let resource = config_resource_specifier(resource_type, resource_name)?;
    let options = AdminOptions::new().request_timeout(Some(request_timeout));
    let resources = smol::block_on(admin.describe_configs([&resource], &options))
        .map_err(|error| super::errors::map_kafka_error(error, "读取 Kafka 配置"))?;
    if resources.len() != 1 {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "读取 Kafka 配置",
            "Kafka 未返回唯一的配置资源",
        )));
    }
    let Some(resource) = resources.into_iter().next() else {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "读取 Kafka 配置",
            "Kafka 未返回配置资源",
        )));
    };
    let resource = resource.map_err(|code| {
        super::errors::map_kafka_error(rdkafka::error::KafkaError::AdminOp(code), "读取 Kafka 配置")
    })?;
    map_config_resource(resource)
}

#[cfg(feature = "cmake-build")]
fn timeout_millis(timeout: Duration) -> i32 {
    i32::try_from(timeout.as_millis())
        .unwrap_or(i32::MAX)
        .max(1)
}

#[cfg(feature = "cmake-build")]
struct NativeConfigResource(*mut rdkafka::types::RDKafkaConfigResource);

#[cfg(feature = "cmake-build")]
impl Drop for NativeConfigResource {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { rdkafka::bindings::rd_kafka_ConfigResource_destroy(self.0) };
        }
    }
}

#[cfg(feature = "cmake-build")]
struct NativeAdminOptions(*mut rdkafka::types::RDKafkaAdminOptions);

#[cfg(feature = "cmake-build")]
impl Drop for NativeAdminOptions {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { rdkafka::bindings::rd_kafka_AdminOptions_destroy(self.0) };
        }
    }
}

#[cfg(feature = "cmake-build")]
struct NativeQueue(*mut rdkafka::types::RDKafkaQueue);

#[cfg(feature = "cmake-build")]
impl Drop for NativeQueue {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { rdkafka::bindings::rd_kafka_queue_destroy(self.0) };
        }
    }
}

#[cfg(feature = "cmake-build")]
struct NativeEvent(*mut rdkafka::types::RDKafkaEvent);

#[cfg(feature = "cmake-build")]
impl Drop for NativeEvent {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { rdkafka::bindings::rd_kafka_event_destroy(self.0) };
        }
    }
}

#[cfg(feature = "cmake-build")]
fn update_config_incrementally(
    admin: &AdminClient<DefaultClientContext>,
    request: &KafkaConfigUpdateRequest,
    request_timeout: Duration,
) -> Result<()> {
    let resource_type = match request.resource_type {
        KafkaConfigResourceType::Topic => {
            rdkafka::types::RDKafkaResourceType::RD_KAFKA_RESOURCE_TOPIC
        }
        KafkaConfigResourceType::Broker => {
            rdkafka::types::RDKafkaResourceType::RD_KAFKA_RESOURCE_BROKER
        }
    };
    let operation = match request.operation {
        KafkaConfigUpdateOperation::Set => {
            rdkafka::bindings::rd_kafka_AlterConfigOpType_t::RD_KAFKA_ALTER_CONFIG_OP_TYPE_SET
        }
        KafkaConfigUpdateOperation::Delete => {
            rdkafka::bindings::rd_kafka_AlterConfigOpType_t::RD_KAFKA_ALTER_CONFIG_OP_TYPE_DELETE
        }
    };
    let resource_name = CString::new(request.resource_name.as_str())
        .map_err(|_| DomainError::InvalidConfig("Kafka 配置资源名称包含 NUL 字符".into()))?;
    let key = CString::new(request.key.as_str())
        .map_err(|_| DomainError::InvalidConfig("Kafka 配置键包含 NUL 字符".into()))?;
    let value = request
        .value
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| DomainError::InvalidConfig("Kafka 配置值包含 NUL 字符".into()))?;
    let resource = unsafe {
        rdkafka::bindings::rd_kafka_ConfigResource_new(resource_type, resource_name.as_ptr())
    };
    if resource.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "修改 Kafka 配置",
            "Kafka 无法创建配置资源请求",
        )));
    }
    let resource = NativeConfigResource(resource);
    let value_ptr = value.as_ref().map_or(ptr::null(), |value| value.as_ptr());
    let add_error = unsafe {
        rdkafka::bindings::rd_kafka_ConfigResource_add_incremental_config(
            resource.0,
            key.as_ptr(),
            operation,
            value_ptr,
        )
    };
    if !add_error.is_null() {
        let code = unsafe { rdkafka::bindings::rd_kafka_error_code(add_error) };
        unsafe { rdkafka::bindings::rd_kafka_error_destroy(add_error) };
        return Err(super::errors::map_kafka_error(
            rdkafka::error::KafkaError::AdminOp(code.into()),
            "修改 Kafka 配置",
        ));
    }

    let client = admin.inner().native_ptr();
    let options = unsafe {
        rdkafka::bindings::rd_kafka_AdminOptions_new(
            client,
            rdkafka::types::RDKafkaAdminOp::RD_KAFKA_ADMIN_OP_INCREMENTALALTERCONFIGS,
        )
    };
    if options.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "修改 Kafka 配置",
            "Kafka 无法创建管理请求选项",
        )));
    }
    let options = NativeAdminOptions(options);
    let timeout_ms = timeout_millis(request_timeout);
    let mut error_text = vec![0 as c_char; 512];
    let options_error = unsafe {
        rdkafka::bindings::rd_kafka_AdminOptions_set_request_timeout(
            options.0,
            timeout_ms,
            error_text.as_mut_ptr(),
            error_text.len(),
        )
    };
    if options_error.is_error() {
        return Err(super::errors::map_kafka_error(
            rdkafka::error::KafkaError::AdminOp(options_error.into()),
            "修改 Kafka 配置",
        ));
    }

    let queue = unsafe { rdkafka::bindings::rd_kafka_queue_get_main(client) };
    if queue.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "修改 Kafka 配置",
            "Kafka 无法获取管理事件队列",
        )));
    }
    let queue = NativeQueue(queue);
    let mut resources = [resource.0];
    unsafe {
        rdkafka::bindings::rd_kafka_IncrementalAlterConfigs(
            client,
            resources.as_mut_ptr(),
            resources.len(),
            options.0,
            queue.0,
        );
    }

    let deadline = Instant::now() + request_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DomainError::Kafka(
                KafkaError::new(
                    KafkaErrorCategory::Timeout,
                    "修改 Kafka 配置",
                    "Kafka 配置请求超时",
                )
                .retryable(true),
            ));
        }
        let event =
            unsafe { rdkafka::bindings::rd_kafka_queue_poll(queue.0, timeout_millis(remaining)) };
        if event.is_null() {
            return Err(DomainError::Kafka(
                KafkaError::new(
                    KafkaErrorCategory::Timeout,
                    "修改 Kafka 配置",
                    "Kafka 配置请求超时",
                )
                .retryable(true),
            ));
        }
        let event = NativeEvent(event);
        let event_type = unsafe { rdkafka::bindings::rd_kafka_event_type(event.0) };
        if event_type != rdkafka::bindings::RD_KAFKA_EVENT_INCREMENTALALTERCONFIGS_RESULT {
            continue;
        }
        let event_error = unsafe { rdkafka::bindings::rd_kafka_event_error(event.0) };
        if event_error.is_error() {
            return Err(super::errors::map_kafka_error(
                rdkafka::error::KafkaError::AdminOp(event_error.into()),
                "修改 Kafka 配置",
            ));
        }
        let result =
            unsafe { rdkafka::bindings::rd_kafka_event_IncrementalAlterConfigs_result(event.0) };
        if result.is_null() {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Protocol,
                "修改 Kafka 配置",
                "Kafka 返回了无效的配置修改结果",
            )));
        }
        let mut count = 0usize;
        let response_resources = unsafe {
            rdkafka::bindings::rd_kafka_IncrementalAlterConfigs_result_resources(result, &mut count)
        };
        if response_resources.is_null() || count != 1 {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Protocol,
                "修改 Kafka 配置",
                "Kafka 未返回唯一的配置修改结果",
            )));
        }
        let response_resource = unsafe { *response_resources };
        if response_resource.is_null() {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Protocol,
                "修改 Kafka 配置",
                "Kafka 返回了空的配置修改结果",
            )));
        }
        let resource_error =
            unsafe { rdkafka::bindings::rd_kafka_ConfigResource_error(response_resource) };
        if resource_error.is_error() {
            return Err(super::errors::map_kafka_error(
                rdkafka::error::KafkaError::AdminOp(resource_error.into()),
                "修改 Kafka 配置",
            ));
        }
        return Ok(());
    }
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
