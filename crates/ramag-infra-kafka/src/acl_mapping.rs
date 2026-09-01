use ramag_domain::entities::{
    KafkaAcl, KafkaAclFilter, KafkaAclOperation, KafkaAclPatternType, KafkaAclPermission,
    KafkaAclResourceType,
};
use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory, Result};
use std::ffi::{CStr, CString, c_char};
use std::ptr;

#[cfg(feature = "cmake-build")]
pub(super) struct NativeAclBinding(pub(super) *mut rdkafka::bindings::rd_kafka_AclBinding_t);

#[cfg(feature = "cmake-build")]
impl Drop for NativeAclBinding {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { rdkafka::bindings::rd_kafka_AclBinding_destroy(self.0) };
        }
    }
}

#[cfg(feature = "cmake-build")]
fn acl_resource_type(
    resource_type: Option<KafkaAclResourceType>,
    operation: &'static str,
) -> Result<rdkafka::bindings::rd_kafka_ResourceType_t> {
    match resource_type.unwrap_or(KafkaAclResourceType::Any) {
        KafkaAclResourceType::Any => {
            Ok(rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_ANY)
        }
        KafkaAclResourceType::Topic => {
            Ok(rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC)
        }
        KafkaAclResourceType::Group => {
            Ok(rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP)
        }
        KafkaAclResourceType::Cluster => {
            Ok(rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER)
        }
        KafkaAclResourceType::TransactionalId => {
            Ok(rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TRANSACTIONAL_ID)
        }
        KafkaAclResourceType::Unknown | KafkaAclResourceType::DelegationToken => {
            Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Unsupported,
                operation,
                "当前 librdkafka 不支持该 Kafka ACL Resource Type",
            )))
        }
    }
}

#[cfg(feature = "cmake-build")]
fn acl_pattern_type(
    pattern_type: Option<ramag_domain::entities::KafkaAclPatternType>,
    operation: &'static str,
) -> Result<rdkafka::bindings::rd_kafka_ResourcePatternType_t> {
    match pattern_type.unwrap_or(ramag_domain::entities::KafkaAclPatternType::Any) {
        ramag_domain::entities::KafkaAclPatternType::Any => {
            Ok(rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_ANY)
        }
        ramag_domain::entities::KafkaAclPatternType::Match => {
            Ok(rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_MATCH)
        }
        ramag_domain::entities::KafkaAclPatternType::Literal => Ok(
            rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_LITERAL,
        ),
        ramag_domain::entities::KafkaAclPatternType::Prefixed => Ok(
            rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_PREFIXED,
        ),
        ramag_domain::entities::KafkaAclPatternType::Unknown => {
            Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::InvalidConfig,
                operation,
                "Kafka ACL Pattern 不能是未知值",
            )))
        }
    }
}

#[cfg(feature = "cmake-build")]
fn acl_operation(
    acl_operation: Option<KafkaAclOperation>,
    operation: &'static str,
) -> Result<rdkafka::bindings::rd_kafka_AclOperation_t> {
    let acl_operation = acl_operation.unwrap_or(KafkaAclOperation::Any);
    let value = match acl_operation {
        KafkaAclOperation::Unknown => {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::InvalidConfig,
                operation,
                "Kafka ACL Operation 不能是未知值",
            )));
        }
        KafkaAclOperation::Any => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ANY
        }
        KafkaAclOperation::All => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALL
        }
        KafkaAclOperation::Read => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_READ
        }
        KafkaAclOperation::Write => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_WRITE
        }
        KafkaAclOperation::Create => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CREATE
        }
        KafkaAclOperation::Delete => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DELETE
        }
        KafkaAclOperation::Alter => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER
        }
        KafkaAclOperation::Describe => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE
        }
        KafkaAclOperation::ClusterAction => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CLUSTER_ACTION
        }
        KafkaAclOperation::DescribeConfigs => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE_CONFIGS
        }
        KafkaAclOperation::AlterConfigs => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER_CONFIGS
        }
        KafkaAclOperation::IdempotentWrite => {
            rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_IDEMPOTENT_WRITE
        }
    };
    Ok(value)
}

#[cfg(feature = "cmake-build")]
fn acl_permission(
    permission: Option<KafkaAclPermission>,
) -> rdkafka::bindings::rd_kafka_AclPermissionType_t {
    match permission {
        None => rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ANY,
        Some(KafkaAclPermission::Allow) => {
            rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ALLOW
        }
        Some(KafkaAclPermission::Deny) => {
            rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_DENY
        }
    }
}

#[cfg(feature = "cmake-build")]
fn native_error_text(buffer: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "cmake-build")]
pub(super) fn new_acl_binding(acl: &KafkaAcl, operation: &'static str) -> Result<NativeAclBinding> {
    let resource_type = acl_resource_type(Some(acl.resource_type), operation)?;
    let pattern_type = acl_pattern_type(Some(acl.pattern_type), operation)?;
    let acl_operation = acl_operation(Some(acl.operation), operation)?;
    let principal = CString::new(acl.principal.as_str())
        .map_err(|_| DomainError::InvalidConfig("ACL Principal 包含 NUL 字符".into()))?;
    let host = CString::new(acl.host.as_str())
        .map_err(|_| DomainError::InvalidConfig("ACL Host 包含 NUL 字符".into()))?;
    let resource_name = CString::new(acl.resource_name.as_str())
        .map_err(|_| DomainError::InvalidConfig("ACL Resource Name 包含 NUL 字符".into()))?;
    let mut error_text = vec![0 as c_char; 512];
    let binding = unsafe {
        rdkafka::bindings::rd_kafka_AclBinding_new(
            resource_type,
            resource_name.as_ptr(),
            pattern_type,
            principal.as_ptr(),
            host.as_ptr(),
            acl_operation,
            match acl.permission {
                KafkaAclPermission::Allow => rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ALLOW,
                KafkaAclPermission::Deny => rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_DENY,
            },
            error_text.as_mut_ptr(),
            error_text.len(),
        )
    };
    if binding.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::InvalidConfig,
            operation,
            format!("Kafka ACL 参数无效：{}", native_error_text(&error_text)),
        )));
    }
    Ok(NativeAclBinding(binding))
}

#[cfg(feature = "cmake-build")]
pub(super) fn new_acl_filter(
    filter: &KafkaAclFilter,
    operation: &'static str,
) -> Result<NativeAclBinding> {
    let resource_type = acl_resource_type(filter.resource_type, operation)?;
    let pattern_type = acl_pattern_type(filter.pattern_type, operation)?;
    let acl_operation = acl_operation(filter.operation, operation)?;
    let principal = filter
        .principal
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| DomainError::InvalidConfig("ACL Principal 包含 NUL 字符".into()))?;
    let host = filter
        .host
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| DomainError::InvalidConfig("ACL Host 包含 NUL 字符".into()))?;
    let resource_name = filter
        .resource_name
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| DomainError::InvalidConfig("ACL Resource Name 包含 NUL 字符".into()))?;
    let mut error_text = vec![0 as c_char; 512];
    let binding = unsafe {
        rdkafka::bindings::rd_kafka_AclBindingFilter_new(
            resource_type,
            resource_name
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            pattern_type,
            principal
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            host.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
            acl_operation,
            acl_permission(filter.permission),
            error_text.as_mut_ptr(),
            error_text.len(),
        )
    };
    if binding.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::InvalidConfig,
            operation,
            format!("Kafka ACL 查询条件无效：{}", native_error_text(&error_text)),
        )));
    }
    Ok(NativeAclBinding(binding))
}

#[cfg(feature = "cmake-build")]
fn native_acl_string(
    value: *const c_char,
    field: &'static str,
    operation: &'static str,
) -> Result<String> {
    if value.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            operation,
            format!("Kafka ACL 返回了空的 {field}"),
        )));
    }
    Ok(unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned())
}

#[cfg(feature = "cmake-build")]
pub(super) fn map_native_acl(
    acl: *const rdkafka::bindings::rd_kafka_AclBinding_t,
    operation: &'static str,
) -> Result<KafkaAcl> {
    if acl.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            operation,
            "Kafka 返回了空的 ACL 规则",
        )));
    }
    let resource_type = match unsafe { rdkafka::bindings::rd_kafka_AclBinding_restype(acl) } {
        rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TOPIC => {
            KafkaAclResourceType::Topic
        }
        rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_GROUP => {
            KafkaAclResourceType::Group
        }
        rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_BROKER => {
            KafkaAclResourceType::Cluster
        }
        rdkafka::bindings::rd_kafka_ResourceType_t::RD_KAFKA_RESOURCE_TRANSACTIONAL_ID => {
            KafkaAclResourceType::TransactionalId
        }
        _ => {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Unsupported,
                operation,
                "Kafka 返回了当前客户端不支持的 ACL Resource Type",
            )));
        }
    };
    let pattern_type = match unsafe {
        rdkafka::bindings::rd_kafka_AclBinding_resource_pattern_type(acl)
    } {
        rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_LITERAL => {
            KafkaAclPatternType::Literal
        }
        rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_PREFIXED => {
            KafkaAclPatternType::Prefixed
        }
        rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_MATCH => {
            KafkaAclPatternType::Match
        }
        rdkafka::bindings::rd_kafka_ResourcePatternType_t::RD_KAFKA_RESOURCE_PATTERN_ANY => {
            KafkaAclPatternType::Any
        }
        _ => KafkaAclPatternType::Unknown,
    };
    let acl_operation = match unsafe { rdkafka::bindings::rd_kafka_AclBinding_operation(acl) } {
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ANY => {
            KafkaAclOperation::Any
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALL => {
            KafkaAclOperation::All
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_READ => {
            KafkaAclOperation::Read
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_WRITE => {
            KafkaAclOperation::Write
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CREATE => {
            KafkaAclOperation::Create
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DELETE => {
            KafkaAclOperation::Delete
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER => {
            KafkaAclOperation::Alter
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE => {
            KafkaAclOperation::Describe
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_CLUSTER_ACTION => {
            KafkaAclOperation::ClusterAction
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_DESCRIBE_CONFIGS => {
            KafkaAclOperation::DescribeConfigs
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_ALTER_CONFIGS => {
            KafkaAclOperation::AlterConfigs
        }
        rdkafka::bindings::rd_kafka_AclOperation_t::RD_KAFKA_ACL_OPERATION_IDEMPOTENT_WRITE => {
            KafkaAclOperation::IdempotentWrite
        }
        _ => KafkaAclOperation::Unknown,
    };
    let permission = match unsafe { rdkafka::bindings::rd_kafka_AclBinding_permission_type(acl) } {
        rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_ALLOW => {
            KafkaAclPermission::Allow
        }
        rdkafka::bindings::rd_kafka_AclPermissionType_t::RD_KAFKA_ACL_PERMISSION_TYPE_DENY => {
            KafkaAclPermission::Deny
        }
        _ => {
            return Err(DomainError::Kafka(KafkaError::new(
                KafkaErrorCategory::Protocol,
                operation,
                "Kafka 返回了无效的 ACL Permission",
            )));
        }
    };
    let acl = KafkaAcl {
        principal: native_acl_string(
            unsafe { rdkafka::bindings::rd_kafka_AclBinding_principal(acl) },
            "Principal",
            operation,
        )?,
        host: native_acl_string(
            unsafe { rdkafka::bindings::rd_kafka_AclBinding_host(acl) },
            "Host",
            operation,
        )?,
        resource_type,
        resource_name: native_acl_string(
            unsafe { rdkafka::bindings::rd_kafka_AclBinding_name(acl) },
            "Resource Name",
            operation,
        )?,
        pattern_type,
        operation: acl_operation,
        permission,
    };
    acl.validate().map_err(DomainError::InvalidConfig)?;
    Ok(acl)
}
