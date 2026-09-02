use super::KafkaView;

/// 将可重试的 Kafka 网络错误转换为连接恢复提示；认证、权限和参数错误不触发全局断线状态。
pub(super) fn runtime_recovery_message(
    operation: &str,
    error: &ramag_domain::error::DomainError,
) -> Option<String> {
    let ramag_domain::error::DomainError::Kafka(error) = error else {
        return None;
    };
    error.retryable.then(|| {
        format!(
            "{operation}失败：{}；请检查 Kafka 连接后刷新元数据",
            error.user_message()
        )
    })
}

impl KafkaView {
    /// 记录读取或管理请求的可恢复连接故障，但不自动重放可能产生副作用的写操作。
    pub(super) fn mark_runtime_failure(
        &mut self,
        operation: &str,
        error: &ramag_domain::error::DomainError,
    ) {
        if let Some(message) = runtime_recovery_message(operation, error) {
            self.runtime_error = Some(message);
        }
    }
}
