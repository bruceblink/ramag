//! 领域层统一错误类型。

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DomainError>;

/// 只读保护的统一用户提示；具体拦截原因仅写入日志。
pub const READ_ONLY_MESSAGE: &str = "只读模式下无法执行写操作";

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("配置无效: {0}")]
    InvalidConfig(String),

    #[error("连接失败: {0}")]
    ConnectionFailed(String),

    #[error("查询执行失败: {0}")]
    QueryFailed(String),

    #[error("存储错误: {0}")]
    Storage(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("功能尚未实现: {0}")]
    NotImplemented(String),

    /// 不添加分类前缀，消息可直接用于用户提示。
    #[error("{0}")]
    Forbidden(String),

    #[error("未知错误: {0}")]
    Other(String),
}

impl DomainError {
    /// 返回不含错误分类前缀的消息。
    pub fn message(&self) -> &str {
        match self {
            DomainError::InvalidConfig(m)
            | DomainError::ConnectionFailed(m)
            | DomainError::QueryFailed(m)
            | DomainError::Storage(m)
            | DomainError::NotFound(m)
            | DomainError::NotImplemented(m)
            | DomainError::Forbidden(m)
            | DomainError::Other(m) => m,
        }
    }

    /// 只读拦截直接返回统一提示，其他错误补充操作上下文。
    pub fn write_hint(&self, prefix: &str) -> String {
        match self {
            DomainError::Forbidden(msg) => msg.clone(),
            other => format!("{prefix}：{other}"),
        }
    }
}
