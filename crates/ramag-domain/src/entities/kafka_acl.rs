use serde::{Deserialize, Serialize};

use super::kafka_validation::validate_required_text;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclPatternType {
    Unknown,
    Any,
    Match,
    Literal,
    Prefixed,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KafkaAclPermission {
    Allow,
    Deny,
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
    /// 校验完整 ACL 规则；空资源名被拒绝，避免误把模糊筛选条件当作删除目标。
    pub fn validate(&self) -> Result<(), String> {
        validate_required_text("ACL Principal", &self.principal, MAX_KAFKA_PRINCIPAL_BYTES)?;
        validate_required_text("ACL Host", &self.host, MAX_KAFKA_ACL_HOST_BYTES)?;
        validate_required_text(
            "ACL Resource Name",
            &self.resource_name,
            MAX_KAFKA_ACL_RESOURCE_NAME_BYTES,
        )?;
        Ok(())
    }
}
