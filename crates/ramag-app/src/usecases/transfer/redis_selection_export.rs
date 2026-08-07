//! Redis 单个 key 或命名空间前缀导出。
//!
//! 文件沿用整 DB JSONL 记录格式，因此类型、TTL、二进制值和大集合续片都能直接恢复；
//! 首行额外记录 key 或 prefix 范围，导入端据此拒绝范围外记录。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConnectionConfig, ProgressFn, TransferSummary, validate_redis_match_pattern,
};
use ramag_domain::error::{DomainError, Result};
use serde_json::json;

use super::redis::{
    ExportKeySource, KeyOutcome, PAGE_ITEMS, ensure_redis, export_key, export_scanned_keys,
};
use super::{Reporter, finish_summary, is_cancelled, with_export_sink};
use crate::usecases::RedisService;

pub async fn export_redis_key(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    key: &str,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_redis(config)?;

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        reporter.snapshot.objects_total = Some(1);
        reporter.stage("导出 key", key);
        write_header(&mut sink, db, "key", key)?;
        if is_cancelled(cancel) {
            summary.cancelled = true;
            return Ok(finish_summary(summary, start));
        }

        let pages = svc
            .read_value_first_pages(config, db, &[key.to_string()], PAGE_ITEMS)
            .await?;
        let page = pages
            .into_iter()
            .next()
            .ok_or_else(|| DomainError::QueryFailed("Redis 单 Key 首页读取结果为空".into()))?;
        let source = ExportKeySource { svc, config, db };
        let mut line = Vec::with_capacity(64 * 1024);
        match export_key(
            &source,
            key,
            page,
            cancel,
            &mut sink,
            &mut line,
            &mut summary,
        )
        .await?
        {
            KeyOutcome::Exported => summary.objects = 1,
            KeyOutcome::Vanished => {
                return Err(DomainError::NotFound(format!(
                    "Key「{key}」已不存在或在导出前过期"
                )));
            }
        }
        if summary.cancelled {
            return Ok(finish_summary(summary, start));
        }

        summary.bytes = sink.bytes_written();
        reporter.snapshot.objects_done = 1;
        reporter.snapshot.items_done = summary.items;
        reporter.snapshot.bytes = summary.bytes;
        reporter.emit();
        sink.finish()?;
        Ok(finish_summary(summary, start))
    })
    .await
}

pub async fn export_redis_prefix(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    prefix: &str,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_redis(config)?;
    let pattern = format!("{}:*", escape_glob_literal(prefix));
    validate_redis_match_pattern(&pattern)?;

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        reporter.stage("扫描前缀", prefix);
        write_header(&mut sink, db, "prefix", prefix)?;

        let source = ExportKeySource { svc, config, db };
        let mut line = Vec::with_capacity(64 * 1024);
        let vanished = export_scanned_keys(
            &source,
            Some(&pattern),
            cancel,
            &mut sink,
            &mut line,
            &mut summary,
            &mut reporter,
        )
        .await?;
        if summary.cancelled {
            return Ok(finish_summary(summary, start));
        }
        if vanished > 0 {
            summary.push_warning(format!(
                "{vanished} 个 key 在导出期间消失（并发删除 / 过期）"
            ));
        }

        summary.bytes = sink.bytes_written();
        reporter.snapshot.bytes = summary.bytes;
        reporter.emit();
        sink.finish()?;
        Ok(finish_summary(summary, start))
    })
    .await
}

fn write_header(sink: &mut super::ExportSink, db: u8, scope: &str, object: &str) -> Result<()> {
    sink.write_str(&format!(
        "{}\n",
        json!({
            "ramag_export": 1,
            "engine": "redis",
            "db": db,
            "scope": scope,
            "object": object,
        })
    ))
}

/// 转义 glob 字符，确保命名空间按字面前缀匹配。
fn escape_glob_literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_pattern_escapes_glob_metacharacters() {
        assert_eq!(escape_glob_literal("user"), "user");
        assert_eq!(escape_glob_literal("a*b?[c]\\d"), "a\\*b\\?\\[c\\]\\\\d");
    }
}
