//! OpenDAL 与本地 I/O 错误的脱敏映射。

use std::io;

use opendal::ErrorKind;
use ramag_domain::error::{ObjectStorageError, ObjectStorageErrorCategory};

pub fn invalid(operation: &'static str, message: impl Into<String>) -> ObjectStorageError {
    ObjectStorageError::new(
        ObjectStorageErrorCategory::InvalidConfig,
        operation,
        message,
    )
}

pub fn cancelled(operation: &'static str) -> ObjectStorageError {
    ObjectStorageError::new(
        ObjectStorageErrorCategory::Cancelled,
        operation,
        "操作已取消",
    )
}

pub fn conflict(operation: &'static str, message: impl Into<String>) -> ObjectStorageError {
    ObjectStorageError::new(ObjectStorageErrorCategory::Conflict, operation, message)
}

pub fn map_opendal(operation: &'static str, error: opendal::Error) -> ObjectStorageError {
    let transport_category = classify_transport_error(&error);
    let provider_code = extract_provider_field(error.message(), "code");
    let request_id = extract_provider_field(error.message(), "request_id");
    let normalized_code = provider_code.as_deref().unwrap_or("").to_ascii_lowercase();
    let provider_category = match normalized_code.as_str() {
        "invalidaccesskeyid" | "signaturedoesnotmatch" | "authfailure" => {
            Some(ObjectStorageErrorCategory::InvalidCredentials)
        }
        "invalidobjectstate" | "objectnotappendable" | "operationnotpermitted" => {
            Some(ObjectStorageErrorCategory::Archived)
        }
        "requesttimetoolskewed" | "requesttimetooskewed" | "requesthasexpired" => {
            Some(ObjectStorageErrorCategory::ClockSkew)
        }
        _ => None,
    };
    let (mut category, mut message, retryable) = match error.kind() {
        ErrorKind::NotFound => (
            ObjectStorageErrorCategory::NotFound,
            "对象不存在或已被删除",
            false,
        ),
        ErrorKind::PermissionDenied => (
            ObjectStorageErrorCategory::PermissionDenied,
            "当前凭据没有执行此操作的权限",
            false,
        ),
        ErrorKind::AlreadyExists | ErrorKind::ConditionNotMatch => (
            ObjectStorageErrorCategory::Conflict,
            "目标已存在或已被其他操作修改",
            false,
        ),
        ErrorKind::RateLimited => (
            ObjectStorageErrorCategory::RateLimited,
            "云服务请求过于频繁，请稍后重试",
            true,
        ),
        ErrorKind::Unexpected => (
            ObjectStorageErrorCategory::Provider,
            "云服务暂时无法完成该操作",
            error.is_temporary(),
        ),
        _ => (
            ObjectStorageErrorCategory::Provider,
            "云服务返回了无法处理的结果",
            error.is_temporary(),
        ),
    };
    if let Some(provider_category) = provider_category {
        category = provider_category;
        message = match provider_category {
            ObjectStorageErrorCategory::InvalidCredentials => "访问凭据无效或签名不匹配",
            ObjectStorageErrorCategory::Archived => "对象处于归档状态，需要先恢复",
            ObjectStorageErrorCategory::ClockSkew => "本机时间与云服务偏差过大",
            _ => message,
        };
    } else if let Some(transport_category) = transport_category {
        category = transport_category;
        message = match transport_category {
            ObjectStorageErrorCategory::Timeout => "连接云服务超时",
            ObjectStorageErrorCategory::Tls => "云服务 TLS 证书校验失败",
            ObjectStorageErrorCategory::Network => "无法连接云服务",
            _ => message,
        };
    }
    ObjectStorageError::new(category, operation, message)
        .with_provider_details(provider_code, request_id)
        .retryable(retryable)
}

fn classify_transport_error(error: &opendal::Error) -> Option<ObjectStorageErrorCategory> {
    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(reqwest_error) = current.downcast_ref::<reqwest::Error>() {
            if reqwest_error.is_timeout() {
                return Some(ObjectStorageErrorCategory::Timeout);
            }
            let detail = reqwest_error.to_string().to_ascii_lowercase();
            if detail.contains("tls")
                || detail.contains("certificate")
                || detail.contains("cert ")
                || detail.contains("rustls")
            {
                return Some(ObjectStorageErrorCategory::Tls);
            }
            if reqwest_error.is_connect() || reqwest_error.is_request() {
                return Some(ObjectStorageErrorCategory::Network);
            }
        }
        source = current.source();
    }
    None
}

fn extract_provider_field(message: &str, field: &str) -> Option<String> {
    let debug_prefix = format!("{field}: \"");
    let xml_prefix = format!("<{}>", to_pascal_case(field));
    let xml_suffix = format!("</{}>", to_pascal_case(field));
    let value = message
        .split_once(&debug_prefix)
        .and_then(|(_, tail)| tail.split_once('"').map(|(value, _)| value))
        .or_else(|| {
            message
                .split_once(&xml_prefix)
                .and_then(|(_, tail)| tail.split_once(&xml_suffix).map(|(value, _)| value))
        })?;
    (!value.is_empty()
        && value.len() <= 256
        && value.is_ascii()
        && !value.chars().any(char::is_control))
    .then(|| value.to_string())
}

fn to_pascal_case(field: &str) -> String {
    field
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

pub fn map_io(operation: &'static str, error: io::Error) -> ObjectStorageError {
    let (category, message) = match error.kind() {
        io::ErrorKind::NotFound => (ObjectStorageErrorCategory::NotFound, "本地文件不存在"),
        io::ErrorKind::PermissionDenied => (
            ObjectStorageErrorCategory::PermissionDenied,
            "没有访问本地文件的权限",
        ),
        io::ErrorKind::AlreadyExists => {
            (ObjectStorageErrorCategory::Conflict, "本地目标文件已存在")
        }
        _ => (ObjectStorageErrorCategory::Provider, "本地文件操作失败"),
    };
    ObjectStorageError::new(category, operation, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_bounded_provider_diagnostics() {
        let message = r#"OssError { code: "InvalidObjectState", message: "detail", request_id: "req-123", host_id: "host" }"#;
        assert_eq!(
            extract_provider_field(message, "code").as_deref(),
            Some("InvalidObjectState")
        );
        assert_eq!(
            extract_provider_field(message, "request_id").as_deref(),
            Some("req-123")
        );
        assert!(extract_provider_field("code: \"line\nbreak\"", "code").is_none());
    }
}
