use serde::{Deserialize, Serialize};

use super::kafka_validation::{validate_optional_single_line, validate_required_text};
use super::{
    MAX_KAFKA_ACL_HOST_BYTES, MAX_KAFKA_ACL_RESOURCE_NAME_BYTES, MAX_KAFKA_PRINCIPAL_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclResourceType {
    Unknown,
    Any,
    Topic,
    Group,
    Cluster,
    TransactionalId,
    DelegationToken,
}

impl KafkaAclResourceType {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Any => "任意",
            Self::Topic => "Topic",
            Self::Group => "Group",
            Self::Cluster => "Cluster",
            Self::TransactionalId => "Transactional ID",
            Self::DelegationToken => "Delegation Token",
        }
    }

    /// librdkafka 将 Kafka ACL 的 Cluster 资源映射为 `BROKER`，其余资源类型不能创建。
    pub const fn supports_binding(self) -> bool {
        matches!(
            self,
            Self::Topic | Self::Group | Self::Cluster | Self::TransactionalId
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclPatternType {
    Unknown,
    Any,
    Match,
    Literal,
    Prefixed,
}

impl KafkaAclPatternType {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Any => "任意",
            Self::Match => "Match",
            Self::Literal => "Literal",
            Self::Prefixed => "Prefixed",
        }
    }

    pub const fn supports_binding(self) -> bool {
        matches!(self, Self::Literal | Self::Prefixed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclOperation {
    Unknown,
    Any,
    All,
    Read,
    Write,
    Create,
    Delete,
    Alter,
    Describe,
    ClusterAction,
    DescribeConfigs,
    AlterConfigs,
    IdempotentWrite,
}

impl KafkaAclOperation {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Any => "任意",
            Self::All => "ALL",
            Self::Read => "READ",
            Self::Write => "WRITE",
            Self::Create => "CREATE",
            Self::Delete => "DELETE",
            Self::Alter => "ALTER",
            Self::Describe => "DESCRIBE",
            Self::ClusterAction => "CLUSTER_ACTION",
            Self::DescribeConfigs => "DESCRIBE_CONFIGS",
            Self::AlterConfigs => "ALTER_CONFIGS",
            Self::IdempotentWrite => "IDEMPOTENT_WRITE",
        }
    }

    pub const fn supports_binding(self) -> bool {
        !matches!(self, Self::Unknown | Self::Any)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclPermission {
    Allow,
    Deny,
}

impl KafkaAclPermission {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Allow => "ALLOW",
            Self::Deny => "DENY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaAcl {
    pub principal: String,
    pub host: String,
    pub resource_type: KafkaAclResourceType,
    pub resource_name: String,
    pub pattern_type: KafkaAclPatternType,
    pub operation: KafkaAclOperation,
    pub permission: KafkaAclPermission,
}

impl KafkaAcl {
    /// 创建 ACL 规则；Host 默认使用 Kafka 的通配值 `*`。
    pub fn new(
        principal: impl Into<String>,
        resource_type: KafkaAclResourceType,
        resource_name: impl Into<String>,
        pattern_type: KafkaAclPatternType,
        operation: KafkaAclOperation,
        permission: KafkaAclPermission,
    ) -> Self {
        Self {
            principal: principal.into(),
            host: "*".into(),
            resource_type,
            resource_name: resource_name.into(),
            pattern_type,
            operation,
            permission,
        }
    }

    /// 校验完整 ACL 规则；空资源名被拒绝，避免误把模糊筛选条件当作删除目标。
    pub fn validate(&self) -> Result<(), String> {
        validate_required_text("ACL Principal", &self.principal, MAX_KAFKA_PRINCIPAL_BYTES)?;
        validate_required_text("ACL Host", &self.host, MAX_KAFKA_ACL_HOST_BYTES)?;
        validate_required_text(
            "ACL Resource Name",
            &self.resource_name,
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
        )?;
        if !self.resource_type.supports_binding() {
            return Err(format!(
                "ACL Resource Type 不支持创建或删除：{}",
                self.resource_type.label()
            ));
        }
        if !self.pattern_type.supports_binding() {
            return Err(format!(
                "ACL Pattern 不支持创建或删除：{}",
                self.pattern_type.label()
            ));
        }
        if !self.operation.supports_binding() {
            return Err(format!(
                "ACL Operation 不支持创建或删除：{}",
                self.operation.label()
            ));
        }
        Ok(())
    }
}

/// Kafka ACL 查询条件；未填写的字段交给 Broker 按任意值匹配。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KafkaAclFilter {
    pub principal: Option<String>,
    pub host: Option<String>,
    pub resource_type: Option<KafkaAclResourceType>,
    pub resource_name: Option<String>,
    pub pattern_type: Option<KafkaAclPatternType>,
    pub operation: Option<KafkaAclOperation>,
    pub permission: Option<KafkaAclPermission>,
}

impl KafkaAclFilter {
    /// 校验查询条件；过滤条件允许为空，但不能把未知枚举值传给 Broker。
    pub fn validate(&self) -> Result<(), String> {
        validate_optional_single_line(
            "ACL Principal",
            self.principal.as_deref(),
            MAX_KAFKA_PRINCIPAL_BYTES,
        )?;
        validate_optional_single_line("ACL Host", self.host.as_deref(), MAX_KAFKA_ACL_HOST_BYTES)?;
        validate_optional_single_line(
            "ACL Resource Name",
            self.resource_name.as_deref(),
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
        )?;
        if matches!(self.resource_type, Some(KafkaAclResourceType::Unknown)) {
            return Err("ACL Resource Type 不能是未知值".into());
        }
        if matches!(self.pattern_type, Some(KafkaAclPatternType::Unknown)) {
            return Err("ACL Pattern 不能是未知值".into());
        }
        if matches!(self.operation, Some(KafkaAclOperation::Unknown)) {
            return Err("ACL Operation 不能是未知值".into());
        }
        Ok(())
    }
}
