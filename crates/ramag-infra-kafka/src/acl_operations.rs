use super::acl_mapping;
use super::{NativeAdminOptions, NativeEvent, NativeQueue, timeout_millis};
use ramag_domain::entities::{KafkaAcl, KafkaAclFilter, MAX_KAFKA_ACLS};
use ramag_domain::error::{DomainError, KafkaError, KafkaErrorCategory, Result};
use rdkafka::admin::AdminClient;
use rdkafka::client::DefaultClientContext;
use rdkafka::error::IsError;
use std::ffi::c_char;
use std::time::{Duration, Instant};

#[cfg(feature = "cmake-build")]
fn native_admin_options(
    admin: &AdminClient<DefaultClientContext>,
    admin_operation: rdkafka::types::RDKafkaAdminOp,
    request_timeout: Duration,
    operation: &'static str,
) -> Result<NativeAdminOptions> {
    let options = unsafe {
        rdkafka::bindings::rd_kafka_AdminOptions_new(admin.inner().native_ptr(), admin_operation)
    };
    if options.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            operation,
            "Kafka 无法创建管理请求选项",
        )));
    }
    let options = NativeAdminOptions(options);
    let mut error_text = vec![0 as c_char; 512];
    let result = unsafe {
        rdkafka::bindings::rd_kafka_AdminOptions_set_request_timeout(
            options.0,
            timeout_millis(request_timeout),
            error_text.as_mut_ptr(),
            error_text.len(),
        )
    };
    if result.is_error() {
        return Err(super::super::errors::map_kafka_error(
            rdkafka::error::KafkaError::AdminOp(result.into()),
            operation,
        ));
    }
    Ok(options)
}

#[cfg(feature = "cmake-build")]
fn poll_acl_event(
    queue: &NativeQueue,
    request_timeout: Duration,
    expected_type: i32,
    operation: &'static str,
) -> Result<NativeEvent> {
    let deadline = Instant::now() + request_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DomainError::Kafka(
                KafkaError::new(KafkaErrorCategory::Timeout, operation, "Kafka ACL 请求超时")
                    .retryable(true),
            ));
        }
        let event =
            unsafe { rdkafka::bindings::rd_kafka_queue_poll(queue.0, timeout_millis(remaining)) };
        if event.is_null() {
            return Err(DomainError::Kafka(
                KafkaError::new(KafkaErrorCategory::Timeout, operation, "Kafka ACL 请求超时")
                    .retryable(true),
            ));
        }
        let event = NativeEvent(event);
        if unsafe { rdkafka::bindings::rd_kafka_event_type(event.0) } != expected_type {
            continue;
        }
        let event_error = unsafe { rdkafka::bindings::rd_kafka_event_error(event.0) };
        if event_error.is_error() {
            return Err(super::super::errors::map_kafka_error(
                rdkafka::error::KafkaError::AdminOp(event_error.into()),
                operation,
            ));
        }
        return Ok(event);
    }
}

#[cfg(feature = "cmake-build")]
fn map_native_acl_error(
    error: *const rdkafka::bindings::rd_kafka_error_t,
    operation: &'static str,
) -> Result<()> {
    if error.is_null() {
        return Ok(());
    }
    let code = unsafe { rdkafka::bindings::rd_kafka_error_code(error) };
    Err(super::super::errors::map_kafka_error(
        rdkafka::error::KafkaError::AdminOp(code.into()),
        operation,
    ))
}

#[cfg(feature = "cmake-build")]
pub(super) fn describe_acls_native(
    admin: &AdminClient<DefaultClientContext>,
    filter: &KafkaAclFilter,
    request_timeout: Duration,
) -> Result<Vec<KafkaAcl>> {
    let filter = acl_mapping::new_acl_filter(filter, "读取 Kafka ACL")?;
    let options = native_admin_options(
        admin,
        rdkafka::types::RDKafkaAdminOp::RD_KAFKA_ADMIN_OP_DESCRIBEACLS,
        request_timeout,
        "读取 Kafka ACL",
    )?;
    let client = admin.inner().native_ptr();
    let queue = unsafe { rdkafka::bindings::rd_kafka_queue_get_main(client) };
    if queue.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "读取 Kafka ACL",
            "Kafka 无法获取管理事件队列",
        )));
    }
    let queue = NativeQueue(queue);
    unsafe {
        rdkafka::bindings::rd_kafka_DescribeAcls(client, filter.0, options.0, queue.0);
    }
    let event = poll_acl_event(
        &queue,
        request_timeout,
        rdkafka::bindings::RD_KAFKA_EVENT_DESCRIBEACLS_RESULT,
        "读取 Kafka ACL",
    )?;
    let result = unsafe { rdkafka::bindings::rd_kafka_event_DescribeAcls_result(event.0) };
    if result.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "读取 Kafka ACL",
            "Kafka 返回了无效的 ACL 查询结果",
        )));
    }
    let mut count = 0usize;
    let bindings =
        unsafe { rdkafka::bindings::rd_kafka_DescribeAcls_result_acls(result, &mut count) };
    if count > MAX_KAFKA_ACLS {
        return Err(DomainError::InvalidConfig(format!(
            "Kafka ACL 数量超过 {MAX_KAFKA_ACLS} 个上限"
        )));
    }
    if count > 0 && bindings.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "读取 Kafka ACL",
            "Kafka 返回了空的 ACL 列表",
        )));
    }
    (0..count)
        .map(|index| acl_mapping::map_native_acl(unsafe { *bindings.add(index) }, "读取 Kafka ACL"))
        .collect()
}

#[cfg(feature = "cmake-build")]
pub(super) fn create_acl_native(
    admin: &AdminClient<DefaultClientContext>,
    acl: &KafkaAcl,
    request_timeout: Duration,
) -> Result<()> {
    let binding = acl_mapping::new_acl_binding(acl, "创建 Kafka ACL")?;
    let options = native_admin_options(
        admin,
        rdkafka::types::RDKafkaAdminOp::RD_KAFKA_ADMIN_OP_CREATEACLS,
        request_timeout,
        "创建 Kafka ACL",
    )?;
    let client = admin.inner().native_ptr();
    let queue = unsafe { rdkafka::bindings::rd_kafka_queue_get_main(client) };
    if queue.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "创建 Kafka ACL",
            "Kafka 无法获取管理事件队列",
        )));
    }
    let queue = NativeQueue(queue);
    let mut bindings = [binding.0];
    unsafe {
        rdkafka::bindings::rd_kafka_CreateAcls(
            client,
            bindings.as_mut_ptr(),
            bindings.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll_acl_event(
        &queue,
        request_timeout,
        rdkafka::bindings::RD_KAFKA_EVENT_CREATEACLS_RESULT,
        "创建 Kafka ACL",
    )?;
    let result = unsafe { rdkafka::bindings::rd_kafka_event_CreateAcls_result(event.0) };
    if result.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "创建 Kafka ACL",
            "Kafka 返回了无效的 ACL 创建结果",
        )));
    }
    let mut count = 0usize;
    let results = unsafe { rdkafka::bindings::rd_kafka_CreateAcls_result_acls(result, &mut count) };
    if count != 1 || results.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "创建 Kafka ACL",
            "Kafka 未返回唯一的 ACL 创建结果",
        )));
    }
    let acl_result = unsafe { *results };
    let error = unsafe { rdkafka::bindings::rd_kafka_acl_result_error(acl_result) };
    map_native_acl_error(error, "创建 Kafka ACL")
}

#[cfg(feature = "cmake-build")]
pub(super) fn delete_acl_native(
    admin: &AdminClient<DefaultClientContext>,
    acl: &KafkaAcl,
    request_timeout: Duration,
) -> Result<()> {
    let binding = acl_mapping::new_acl_filter(
        &KafkaAclFilter {
            principal: Some(acl.principal.clone()),
            host: Some(acl.host.clone()),
            resource_type: Some(acl.resource_type),
            resource_name: Some(acl.resource_name.clone()),
            pattern_type: Some(acl.pattern_type),
            operation: Some(acl.operation),
            permission: Some(acl.permission),
        },
        "删除 Kafka ACL",
    )?;
    let options = native_admin_options(
        admin,
        rdkafka::types::RDKafkaAdminOp::RD_KAFKA_ADMIN_OP_DELETEACLS,
        request_timeout,
        "删除 Kafka ACL",
    )?;
    let client = admin.inner().native_ptr();
    let queue = unsafe { rdkafka::bindings::rd_kafka_queue_get_main(client) };
    if queue.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "删除 Kafka ACL",
            "Kafka 无法获取管理事件队列",
        )));
    }
    let queue = NativeQueue(queue);
    let mut bindings = [binding.0];
    unsafe {
        rdkafka::bindings::rd_kafka_DeleteAcls(
            client,
            bindings.as_mut_ptr(),
            bindings.len(),
            options.0,
            queue.0,
        );
    }
    let event = poll_acl_event(
        &queue,
        request_timeout,
        rdkafka::bindings::RD_KAFKA_EVENT_DELETEACLS_RESULT,
        "删除 Kafka ACL",
    )?;
    let result = unsafe { rdkafka::bindings::rd_kafka_event_DeleteAcls_result(event.0) };
    if result.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "删除 Kafka ACL",
            "Kafka 返回了无效的 ACL 删除结果",
        )));
    }
    let mut count = 0usize;
    let responses =
        unsafe { rdkafka::bindings::rd_kafka_DeleteAcls_result_responses(result, &mut count) };
    if count != 1 || responses.is_null() {
        return Err(DomainError::Kafka(KafkaError::new(
            KafkaErrorCategory::Protocol,
            "删除 Kafka ACL",
            "Kafka 未返回唯一的 ACL 删除结果",
        )));
    }
    let response = unsafe { *responses };
    let error = unsafe { rdkafka::bindings::rd_kafka_DeleteAcls_result_response_error(response) };
    map_native_acl_error(error, "删除 Kafka ACL")
}
