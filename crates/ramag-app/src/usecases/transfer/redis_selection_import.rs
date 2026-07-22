//! Redis 单 Key / 前缀结构化文件导入入口。

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, ProgressFn, TransferSummary};
use ramag_domain::error::{DomainError, Result};
use serde_json::Value;

use super::redis::{import_redis_db, parse_export_scope};
use crate::usecases::RedisService;

/// 将 Key / 前缀导出文件恢复到所选 DB；对象名取自文件头。
pub async fn import_redis_selection(
    svc: &RedisService,
    config: &ConnectionConfig,
    target_db: u8,
    path: &Path,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
    let header: Value = serde_json::from_str(line.trim())
        .map_err(|_| DomainError::InvalidConfig("文件首行不是有效的导出头".into()))?;
    if header.get("ramag_export").and_then(Value::as_u64) != Some(1)
        || header.get("engine").and_then(Value::as_str) != Some("redis")
        || parse_export_scope(&header)?.is_none()
    {
        return Err(DomainError::InvalidConfig(
            "请选择由 Ramag 导出的单 Key / 前缀文件".into(),
        ));
    }
    import_redis_db(svc, config, Some(target_db), path, policy, cancel, progress).await
}
