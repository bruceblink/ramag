//! 领域层统一错误类型。

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DomainError>;

/// 只读保护的统一用户提示；具体拦截原因仅写入日志。
pub const READ_ONLY_MESSAGE: &str = "生产模式下无法执行写操作";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectStorageErrorCategory {
    InvalidConfig,
    InvalidCredentials,
    PermissionDenied,
    ClockSkew,
    Network,
    Tls,
    Timeout,
    RateLimited,
    NotFound,
    Conflict,
    Archived,
    Cancelled,
    Provider,
    CorruptResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KafkaErrorCategory {
    InvalidConfig,
    Authentication,
    Tls,
    Timeout,
    PermissionDenied,
    NotFound,
    Unsupported,
    Cancelled,
    Network,
    Protocol,
    Unknown,
}

/// Kafka 基础设施层返回的安全错误；不携带密码、密钥或消息正文。
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{safe_message}")]
pub struct KafkaError {
    pub category: KafkaErrorCategory,
    pub safe_message: String,
    pub retryable: bool,
    pub operation: &'static str,
}

impl KafkaError {
    pub fn new(
        category: KafkaErrorCategory,
        operation: &'static str,
        safe_message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            safe_message: safe_message.into(),
            retryable: false,
            operation,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn user_message(&self) -> &str {
        &self.safe_message
    }
}

/// 对象存储错误只保留可安全展示和记录的结构化字段。
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{safe_message}")]
pub struct ObjectStorageError {
    pub category: ObjectStorageErrorCategory,
    pub safe_message: String,
    pub provider_code: Option<String>,
    pub request_id: Option<String>,
    pub retryable: bool,
    pub operation: &'static str,
}

impl ObjectStorageError {
    pub fn new(
        category: ObjectStorageErrorCategory,
        operation: &'static str,
        safe_message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            safe_message: safe_message.into(),
            provider_code: None,
            request_id: None,
            retryable: false,
            operation,
        }
    }

    pub fn with_provider_details(
        mut self,
        code: Option<String>,
        request_id: Option<String>,
    ) -> Self {
        self.provider_code = code;
        self.request_id = request_id;
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn user_message(&self) -> String {
        let mut message = self.safe_message.clone();
        if let Some(code) = &self.provider_code {
            message.push_str(&format!("（服务商错误码：{code}）"));
        }
        if let Some(request_id) = &self.request_id {
            message.push_str(&format!("（RequestId：{request_id}）"));
        }
        message
    }
}

pub type ObjectStorageResult<T> = std::result::Result<T, ObjectStorageError>;

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

    #[error("对象存储错误: {0}")]
    ObjectStorage(#[from] ObjectStorageError),

    #[error("Kafka 错误: {0}")]
    Kafka(#[from] KafkaError),

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
            DomainError::ObjectStorage(error) => &error.safe_message,
            DomainError::Kafka(error) => &error.safe_message,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            DomainError::ObjectStorage(error) => error.user_message(),
            DomainError::Kafka(error) => error.user_message().to_string(),
            other => other.message().to_string(),
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
