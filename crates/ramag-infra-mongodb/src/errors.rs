//! mongodb::error::Error → DomainError。按 ErrorKind 大类映射，识别认证 / 网络 / 命令等典型场景

use mongodb::error::{Error as MongoError, ErrorKind};
use ramag_domain::error::DomainError;

/// Command 错误的可读文案：受限账号常见的 code 13（Unauthorized）给出清晰权限提示，
/// 其余用服务端 errmsg。独立函数便于测试（CommandError 含私有字段，无法在测试中直接构造）。
fn command_error_detail(code: i32, message: &str) -> String {
    if code == 13 {
        "无权访问该集合，请检查账号权限".to_string()
    } else {
        message.to_string()
    }
}

pub fn map_mongo_error(err: MongoError) -> DomainError {
    let raw = err.to_string();

    // ErrorKind 是 non_exhaustive，必须用 `_` 兜底
    match err.kind.as_ref() {
        ErrorKind::Authentication { .. } => DomainError::ConnectionFailed(format!(
            "认证失败（用户名 / 密码 / authSource 错误）：{raw}"
        )),
        ErrorKind::Io(_) => DomainError::ConnectionFailed(format!("网络 / IO 错误：{raw}")),
        ErrorKind::ServerSelection { .. } => {
            DomainError::ConnectionFailed(format!("无法选择服务端（请检查 host/port/TLS）：{raw}"))
        }
        ErrorKind::ConnectionPoolCleared { .. } => {
            DomainError::ConnectionFailed(format!("连接池被清空：{raw}"))
        }
        ErrorKind::DnsResolve { .. } => {
            DomainError::ConnectionFailed(format!("DNS 解析失败：{raw}"))
        }
        // 只保留可读字段（code / code_name / errmsg），避免 err.to_string() 里
        // RawDocumentBuf 的 hex dump 噪音；受限账号常见的 code 13 给出清晰提示
        ErrorKind::Command(cmd) => DomainError::QueryFailed(format!(
            "命令错误（code={}, name={}）：{}",
            cmd.code,
            cmd.code_name,
            command_error_detail(cmd.code, &cmd.message)
        )),
        ErrorKind::Write(_) => DomainError::QueryFailed(format!("写入错误：{raw}")),
        ErrorKind::BulkWrite(_) => DomainError::QueryFailed(format!("批量写入错误：{raw}")),
        ErrorKind::InvalidArgument { .. } => DomainError::InvalidConfig(format!("参数错误：{raw}")),
        ErrorKind::InvalidResponse { .. } => {
            DomainError::QueryFailed(format!("服务端响应无效：{raw}"))
        }
        ErrorKind::BsonDeserialization(_) | ErrorKind::BsonSerialization(_) => {
            DomainError::QueryFailed(format!("BSON 序列化失败：{raw}"))
        }
        _ => DomainError::Other(format!("mongodb 错误：{raw}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简单 smoke：随便造一个 Mongo error 看映射不 panic
    #[test]
    fn map_does_not_panic() {
        let err: MongoError = MongoError::custom("smoke");
        let mapped = map_mongo_error(err);
        let msg = format!("{mapped}");
        assert!(!msg.is_empty());
    }

    #[test]
    fn command_detail_is_clean_and_hints_unauthorized() {
        // code 13 → 清晰权限提示，不含 RawDocumentBuf hex dump
        assert_eq!(
            command_error_detail(13, "not authorized on ramag_demo to execute command find"),
            "无权访问该集合，请检查账号权限"
        );
        // 其它 code → 用服务端 errmsg 原文
        assert_eq!(command_error_detail(26, "ns not found"), "ns not found");
    }
}
