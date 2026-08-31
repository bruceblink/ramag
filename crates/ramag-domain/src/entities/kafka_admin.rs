use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::kafka_validation::{
    validate_kafka_managed_topic_name, validate_kafka_topic_name, validate_optional_single_line,
    validate_required_text,
};
use super::{
    MAX_KAFKA_CONFIG_ENTRIES, MAX_KAFKA_CONFIG_KEY_BYTES, MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES,
    MAX_KAFKA_CONFIG_VALUE_BYTES, MAX_KAFKA_PARTITIONS, MAX_KAFKA_REPLICAS,
};

/// 创建 Topic 时提交给 Kafka Admin API 的有限请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTopicCreateRequest {
    pub name: String,
    pub partitions: usize,
    pub replication_factor: usize,
}

impl KafkaTopicCreateRequest {
    pub fn new(name: impl Into<String>, partitions: usize, replication_factor: usize) -> Self {
        Self {
            name: name.into(),
            partitions,
            replication_factor,
        }
    }

    /// 校验创建请求，避免把空数量、内部 Topic 或超大请求交给 Broker。
    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_managed_topic_name(&self.name)?;
        validate_positive_limit(
            "Topic Partition 数量",
            self.partitions,
            MAX_KAFKA_PARTITIONS,
        )?;
        validate_positive_limit(
            "Topic 副本因子",
            self.replication_factor,
            MAX_KAFKA_REPLICAS,
        )?;
        Ok(())
    }
}

/// 增加 Topic Partition 时使用的目标总数；Kafka 不允许减少 Partition。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaTopicPartitionExpansion {
    pub name: String,
    pub total_partitions: usize,
}

impl KafkaTopicPartitionExpansion {
    pub fn new(name: impl Into<String>, total_partitions: usize) -> Self {
        Self {
            name: name.into(),
            total_partitions,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_kafka_managed_topic_name(&self.name)?;
        validate_positive_limit(
            "Topic Partition 总数",
            self.total_partitions,
            MAX_KAFKA_PARTITIONS,
        )?;
        Ok(())
    }
}

/// Kafka Admin API 支持查询的配置资源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaConfigResourceType {
    Topic,
    Broker,
}

impl KafkaConfigResourceType {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Topic => "Topic",
            Self::Broker => "Broker",
        }
    }

    /// 校验 Kafka Admin API 使用的资源名称；Broker `-1` 表示所有 Broker 的默认配置。
    pub fn validate_resource_name(self, name: &str) -> Result<(), String> {
        validate_required_text(
            match self {
                Self::Topic => "Topic 名称",
                Self::Broker => "Broker ID",
            },
            name,
            MAX_KAFKA_CONFIG_RESOURCE_NAME_BYTES,
        )?;
        match self {
            Self::Topic => validate_kafka_topic_name(name),
            Self::Broker => {
                let id = name
                    .parse::<i32>()
                    .map_err(|_| "Broker ID 必须是 -1 或非负整数".to_string())?;
                if id < -1 {
                    return Err("Broker ID 必须是 -1 或非负整数".into());
                }
                Ok(())
            }
        }
    }
}

/// Kafka 返回的配置来源；来源决定配置项是否可以由当前资源覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaConfigSource {
    Unknown,
    DynamicTopic,
    DynamicBroker,
    DynamicDefaultBroker,
    StaticBroker,
    Default,
}

impl KafkaConfigSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知来源",
            Self::DynamicTopic => "动态 Topic",
            Self::DynamicBroker => "动态 Broker",
            Self::DynamicDefaultBroker => "动态默认 Broker",
            Self::StaticBroker => "静态 Broker",
            Self::Default => "默认值",
        }
    }

    pub const fn is_dynamic(self) -> bool {
        matches!(
            self,
            Self::DynamicTopic | Self::DynamicBroker | Self::DynamicDefaultBroker
        )
    }
}

/// 单个 Kafka 配置项；敏感配置的 `value` 必须保持为空，只展示可见性标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConfigEntry {
    pub key: String,
    pub value: Option<String>,
    pub source: KafkaConfigSource,
    pub is_read_only: bool,
    pub is_default: bool,
    pub is_sensitive: bool,
}

impl KafkaConfigEntry {
    /// 只有当前资源的动态覆盖项、非敏感项且未被 Broker 标记为只读时才允许编辑。
    pub const fn can_modify(&self, resource_type: KafkaConfigResourceType) -> bool {
        if self.is_read_only || self.is_sensitive {
            return false;
        }
        matches!(
            (resource_type, self.source),
            (
                KafkaConfigResourceType::Topic,
                KafkaConfigSource::DynamicTopic
            ) | (
                KafkaConfigResourceType::Broker,
                KafkaConfigSource::DynamicBroker | KafkaConfigSource::DynamicDefaultBroker
            )
        )
    }

    /// 返回给 UI 的安全显示文本；敏感值永远不回显，未设置值保持明确占位符。
    pub fn display_value(&self) -> String {
        if self.is_sensitive {
            "••••••".into()
        } else {
            self.value.clone().unwrap_or_else(|| "<未设置>".into())
        }
    }

    /// 返回可用于修改请求的当前值；敏感项和缺失值都不能被预填到编辑器。
    pub fn raw_value_for_update(&self) -> Option<&str> {
        if self.is_sensitive {
            None
        } else {
            self.value.as_deref()
        }
    }
}

/// 一个 Kafka Topic 或 Broker 的配置快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConfigResource {
    pub resource_type: KafkaConfigResourceType,
    pub resource_name: String,
    pub entries: Vec<KafkaConfigEntry>,
}

impl KafkaConfigResource {
    pub fn validate(&self) -> Result<(), String> {
        self.resource_type
            .validate_resource_name(&self.resource_name)?;
        if self.entries.len() > MAX_KAFKA_CONFIG_ENTRIES {
            return Err(format!(
                "Kafka 配置项数量超过 {MAX_KAFKA_CONFIG_ENTRIES} 个上限"
            ));
        }

        let mut keys = HashSet::with_capacity(self.entries.len());
        for entry in &self.entries {
            validate_required_text("Kafka 配置键", &entry.key, MAX_KAFKA_CONFIG_KEY_BYTES)?;
            validate_optional_single_line(
                "Kafka 配置值",
                entry.value.as_deref(),
                MAX_KAFKA_CONFIG_VALUE_BYTES,
            )?;
            if !keys.insert(entry.key.as_str()) {
                return Err(format!("Kafka 配置键重复：{}", entry.key));
            }
            if entry.is_sensitive && entry.value.is_some() {
                return Err(format!("敏感 Kafka 配置不能返回明文：{}", entry.key));
            }
        }
        Ok(())
    }

    pub fn entry(&self, key: &str) -> Option<&KafkaConfigEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

/// Kafka 配置的增量变更类型；删除表示恢复该资源的继承或默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KafkaConfigUpdateOperation {
    Set,
    Delete,
}

impl KafkaConfigUpdateOperation {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Set => "设置",
            Self::Delete => "恢复默认",
        }
    }
}

/// 一次只修改一个 Kafka 配置项，避免 UI 误提交未显示的其他配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaConfigUpdateRequest {
    pub resource_type: KafkaConfigResourceType,
    pub resource_name: String,
    pub key: String,
    pub operation: KafkaConfigUpdateOperation,
    pub value: Option<String>,
}

impl KafkaConfigUpdateRequest {
    pub fn set(
        resource_type: KafkaConfigResourceType,
        resource_name: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            resource_type,
            resource_name: resource_name.into(),
            key: key.into(),
            operation: KafkaConfigUpdateOperation::Set,
            value: Some(value.into()),
        }
    }

    pub fn delete(
        resource_type: KafkaConfigResourceType,
        resource_name: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            resource_type,
            resource_name: resource_name.into(),
            key: key.into(),
            operation: KafkaConfigUpdateOperation::Delete,
            value: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.resource_type
            .validate_resource_name(&self.resource_name)?;
        validate_required_text("Kafka 配置键", &self.key, MAX_KAFKA_CONFIG_KEY_BYTES)?;
        match self.operation {
            KafkaConfigUpdateOperation::Set => {
                let value = self
                    .value
                    .as_deref()
                    .ok_or_else(|| "设置 Kafka 配置必须提供值".to_string())?;
                validate_optional_single_line(
                    "Kafka 配置值",
                    Some(value),
                    MAX_KAFKA_CONFIG_VALUE_BYTES,
                )?;
            }
            KafkaConfigUpdateOperation::Delete => {
                if self.value.is_some() {
                    return Err("恢复 Kafka 配置默认值不能携带值".into());
                }
            }
        }
        Ok(())
    }
}

fn validate_positive_limit(label: &str, value: usize, max: usize) -> Result<(), String> {
    if value == 0 {
        return Err(format!("{label}必须大于 0"));
    }
    if value > max {
        return Err(format!("{label}不能超过 {max} 个"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_resource_validation_preserves_source_and_visibility_metadata() {
        let resource = KafkaConfigResource {
            resource_type: KafkaConfigResourceType::Topic,
            resource_name: "events".into(),
            entries: vec![
                KafkaConfigEntry {
                    key: "retention.ms".into(),
                    value: Some("60000".into()),
                    source: KafkaConfigSource::DynamicTopic,
                    is_read_only: false,
                    is_default: false,
                    is_sensitive: false,
                },
                KafkaConfigEntry {
                    key: "ssl.keystore.password".into(),
                    value: None,
                    source: KafkaConfigSource::Default,
                    is_read_only: true,
                    is_default: true,
                    is_sensitive: true,
                },
            ],
        };

        assert!(resource.validate().is_ok());
        assert!(resource.entries[0].can_modify(KafkaConfigResourceType::Topic));
        assert!(!resource.entries[1].can_modify(KafkaConfigResourceType::Topic));
    }

    #[test]
    fn config_update_request_requires_operation_specific_value_shape() {
        assert!(
            KafkaConfigUpdateRequest::set(
                KafkaConfigResourceType::Topic,
                "events",
                "retention.ms",
                "60000"
            )
            .validate()
            .is_ok()
        );
        assert!(
            KafkaConfigUpdateRequest::delete(
                KafkaConfigResourceType::Broker,
                "-1",
                "log.retention.hours"
            )
            .validate()
            .is_ok()
        );

        let mut invalid = KafkaConfigUpdateRequest::delete(
            KafkaConfigResourceType::Topic,
            "events",
            "retention.ms",
        );
        invalid.value = Some("60000".into());
        assert!(invalid.validate().is_err());
        assert!(
            KafkaConfigResourceType::Broker
                .validate_resource_name("-2")
                .is_err()
        );
    }

    #[test]
    fn sensitive_config_values_are_never_displayed_or_prefilled() {
        let entry = KafkaConfigEntry {
            key: "ssl.keystore.password".into(),
            value: None,
            source: KafkaConfigSource::DynamicBroker,
            is_read_only: false,
            is_default: false,
            is_sensitive: true,
        };

        assert_eq!(entry.display_value(), "••••••");
        assert_eq!(entry.raw_value_for_update(), None);
        assert!(!entry.can_modify(KafkaConfigResourceType::Broker));
    }
}
