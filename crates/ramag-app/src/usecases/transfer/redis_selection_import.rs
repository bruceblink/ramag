//! Redis 单 Key / 前缀结构化文件导入入口。

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ramag_domain::entities::{ConflictPolicy, ConnectionConfig, ProgressFn, TransferSummary};
use ramag_domain::error::{DomainError, Result};
use serde_json::Value;
use tracing::{info, warn};

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
    info!(
        operation = "redis_selection_import",
        connection_id = %config.id,
        target_db,
        policy = ?policy,
        path = %path.display(),
        "transfer started"
    );
    let result =
        import_redis_selection_inner(svc, config, target_db, path, policy, cancel, progress).await;
    match &result {
        Ok(summary) => info!(
            operation = "redis_selection_import",
            connection_id = %config.id,
            target_db,
            policy = ?policy,
            path = %path.display(),
            objects = summary.objects,
            items = summary.items,
            bytes = summary.bytes,
            failed = summary.failed,
            skipped = summary.skipped,
            cancelled = summary.cancelled,
            warning_count = summary.warnings.len() as u64 + summary.warnings_overflow,
            elapsed_ms = summary.elapsed_ms,
            "transfer completed"
        ),
        Err(error) => warn!(
            operation = "redis_selection_import",
            connection_id = %config.id,
            target_db,
            policy = ?policy,
            path = %path.display(),
            error = %error,
            "transfer failed"
        ),
    }
    result
}

async fn import_redis_selection_inner(
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
