//! Redis 连接同步：分批扫描、临时 Key 复制、TTL 修正与 RENAMENX 发布。

use std::time::Instant;

use ramag_domain::entities::{
    ConnectionConfig, DataSyncProgress, DataSyncStage, DataSyncSummary, RedisSyncScope, RedisType,
    RedisValue, TRANSFER_BATCH_ITEMS, ValuePageCursor, validate_redis_key,
};
use ramag_domain::error::{DomainError, Result};

use super::gate::DataSyncPermit;
use super::service::{DataSyncService, PreparedDataSync, RedisPreparedPlan};

const REDIS_TEMP_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

pub(super) async fn run_redis_sync(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &RedisPreparedPlan,
    permit: &DataSyncPermit,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let current = service
        .current_redis_target_snapshot(&prepared.target, &plan.scope)
        .await?;
    if current != plan.target_snapshot {
        return Err(DomainError::InvalidConfig(
            "目标 Redis 范围已在预检后变化，请重新预检并确认".into(),
        ));
    }

    let mut progress = DataSyncProgress {
        stage: DataSyncStage::VerifyingTarget,
        objects_total: prepared.report.objects_total,
        ..DataSyncProgress::default()
    };
    service.gate().update_progress(permit, progress.clone());

    match &plan.scope {
        RedisSyncScope::Database {
            source_db,
            target_db,
            ..
        } => {
            scan_and_sync(
                service,
                prepared,
                plan,
                permit,
                *source_db,
                *target_db,
                None,
                &mut progress,
                summary,
            )
            .await?;
        }
        RedisSyncScope::Prefix {
            source_db,
            target_db,
            source_prefix,
            ..
        } => {
            let pattern = redis_literal_prefix_pattern(source_prefix);
            scan_and_sync(
                service,
                prepared,
                plan,
                permit,
                *source_db,
                *target_db,
                Some(&pattern),
                &mut progress,
                summary,
            )
            .await?;
        }
        RedisSyncScope::Keys {
            source_db,
            target_db,
            mappings,
        } => {
            for chunk in mappings.chunks(TRANSFER_BATCH_ITEMS) {
                if permit.cancellation_requested() {
                    summary.cancelled = true;
                    break;
                }
                let keys: Vec<String> =
                    chunk.iter().map(|mapping| mapping.source.clone()).collect();
                sync_key_batch(
                    service,
                    prepared,
                    plan,
                    permit,
                    *source_db,
                    *target_db,
                    &keys,
                    &mut progress,
                    summary,
                )
                .await?;
            }
        }
    }
    progress.stage = if summary.cancelled {
        DataSyncStage::Cancelling
    } else {
        DataSyncStage::Finalizing
    };
    service.gate().update_progress(permit, progress);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn scan_and_sync(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &RedisPreparedPlan,
    permit: &DataSyncPermit,
    source_db: u8,
    target_db: u8,
    pattern: Option<&str>,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let mut cursor = 0u64;
    loop {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Scanning;
        service.gate().update_progress(permit, progress.clone());
        let page = service
            .redis_service()
            .scan_batch(
                &prepared.source,
                source_db,
                cursor,
                pattern,
                None,
                TRANSFER_BATCH_ITEMS as u32,
            )
            .await?;
        let next_cursor = page.cursor;
        let keys: Vec<String> = page.keys.into_iter().map(|meta| meta.key).collect();
        sync_key_batch(
            service, prepared, plan, permit, source_db, target_db, &keys, progress, summary,
        )
        .await?;
        if next_cursor == 0 {
            return Ok(());
        }
        if next_cursor == cursor {
            return Err(DomainError::QueryFailed(
                "Redis SCAN 游标未推进，已停止同步".into(),
            ));
        }
        cursor = next_cursor;
    }
}

#[allow(clippy::too_many_arguments)]
async fn sync_key_batch(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &RedisPreparedPlan,
    permit: &DataSyncPermit,
    source_db: u8,
    target_db: u8,
    source_keys: &[String],
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    if source_keys.is_empty() {
        return Ok(());
    }
    let mut mapped = Vec::with_capacity(source_keys.len());
    for source_key in source_keys {
        validate_redis_key(source_key)?;
        let target_key = plan.scope.map_key(source_key).ok_or_else(|| {
            DomainError::InvalidConfig(format!("源 Key 不在已确认映射范围内：{source_key}"))
        })?;
        validate_redis_key(&target_key)?;
        mapped.push(target_key);
    }
    let exists = service
        .redis_service()
        .keys_exist(&prepared.target, target_db, &mapped)
        .await?;
    if exists.len() != source_keys.len() {
        return Err(DomainError::QueryFailed(
            "Redis 批量存在性检查应答数量异常".into(),
        ));
    }

    progress.add_scanned(source_keys.len() as u64);
    summary.scanned = summary.scanned.saturating_add(source_keys.len() as u64);
    for ((source_key, target_key), target_exists) in source_keys.iter().zip(&mapped).zip(exists) {
        progress.object = format!("{source_key} → {target_key}");
        if target_exists {
            progress.add_skipped(1);
            summary.skipped = summary.skipped.saturating_add(1);
            progress.objects_done = progress.objects_done.saturating_add(1);
            summary.objects = summary.objects.saturating_add(1);
            service.gate().update_progress(permit, progress.clone());
            continue;
        }
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Writing;
        service.gate().update_progress(permit, progress.clone());
        match copy_key_atomically(
            service, prepared, permit, source_db, target_db, source_key, target_key, progress,
        )
        .await?
        {
            CopyKeyOutcome::Inserted => {
                summary.inserted = summary.inserted.saturating_add(1);
                progress.add_inserted(1);
            }
            CopyKeyOutcome::Skipped => {
                summary.skipped = summary.skipped.saturating_add(1);
                progress.add_skipped(1);
            }
            CopyKeyOutcome::Cancelled => {
                summary.cancelled = true;
                return Ok(());
            }
        }
        progress.objects_done = progress.objects_done.saturating_add(1);
        summary.objects = summary.objects.saturating_add(1);
        summary.bytes = progress.bytes;
        service.gate().update_progress(permit, progress.clone());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyKeyOutcome {
    Inserted,
    Skipped,
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
async fn copy_key_atomically(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    permit: &DataSyncPermit,
    source_db: u8,
    target_db: u8,
    source_key: &str,
    target_key: &str,
    progress: &mut DataSyncProgress,
) -> Result<CopyKeyOutcome> {
    let temp_id = ramag_domain::entities::DataSyncTaskId::new();
    let temp_key = format!("__ramag_sync_tmp__:{}", temp_id.0.simple());
    validate_redis_key(&temp_key)?;

    let result = copy_to_temp(
        service, prepared, permit, source_db, target_db, source_key, target_key, &temp_key,
        progress,
    )
    .await;
    match result {
        Ok(outcome) => Ok(outcome),
        Err(original) => {
            match cleanup_temp(service, &prepared.target, target_db, &temp_key).await {
                Ok(()) => Err(original),
                Err(cleanup) => Err(DomainError::Other(format!(
                    "{original}；同时清理 Redis 临时 Key 失败：{cleanup}"
                ))),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn copy_to_temp(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    permit: &DataSyncPermit,
    source_db: u8,
    target_db: u8,
    source_key: &str,
    target_key: &str,
    temp_key: &str,
    progress: &mut DataSyncProgress,
) -> Result<CopyKeyOutcome> {
    let redis = service.redis_service();
    if redis
        .key_type(&prepared.target, target_db, temp_key)
        .await?
        != RedisType::None
    {
        return Err(DomainError::Other(
            "随机 Redis 临时 Key 发生冲突，请重试".into(),
        ));
    }

    let mut cursor = ValuePageCursor::Start;
    let mut kind = None;
    let mut temp_created = false;
    loop {
        if permit.cancellation_requested() {
            cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
            return Ok(CopyKeyOutcome::Cancelled);
        }
        let page = redis
            .read_value_page(
                &prepared.source,
                source_db,
                source_key,
                kind,
                cursor,
                TRANSFER_BATCH_ITEMS as u32,
            )
            .await?;
        if page.skipped > 0 {
            return Err(DomainError::InvalidConfig(format!(
                "Redis Key {source_key} 含当前版本无法保真的二进制字段，已停止同步"
            )));
        }
        let page_kind = redis_value_kind(&page.items)?;
        if page_kind == RedisType::None || page.ttl_ms == Some(-2) {
            cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
            return Ok(CopyKeyOutcome::Skipped);
        }
        if let Some(expected) = kind {
            if page_kind != expected {
                return Err(DomainError::QueryFailed(format!(
                    "Redis Key {source_key} 在读取期间改变类型"
                )));
            }
        } else {
            kind = Some(page_kind);
        }

        if page_has_payload(&page.items) {
            redis
                .write_value_items(&prepared.target, target_db, temp_key, &page.items)
                .await?;
            temp_created = true;
            progress.add_bytes(redis_value_bytes(&page.items));
            if !redis
                .set_ttl_ms(&prepared.target, target_db, temp_key, REDIS_TEMP_TTL_MS)
                .await?
            {
                return Err(DomainError::QueryFailed(format!(
                    "Redis 临时 Key {temp_key} 写入后不存在"
                )));
            }
            service.gate().update_progress(permit, progress.clone());
        }
        let Some(next) = page.next else {
            break;
        };
        cursor = next;
    }

    if !temp_created {
        return Ok(CopyKeyOutcome::Skipped);
    }
    let expected_kind = kind.ok_or_else(|| {
        DomainError::QueryFailed(format!("Redis Key {source_key} 未返回可复制类型"))
    })?;
    let (final_kind, final_ttl) = redis
        .key_type_and_ttl(&prepared.source, source_db, source_key)
        .await?;
    let ttl_observed_at = Instant::now();
    if final_kind == RedisType::None || final_ttl == -2 {
        cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
        return Ok(CopyKeyOutcome::Skipped);
    }
    if final_kind != expected_kind {
        return Err(DomainError::QueryFailed(format!(
            "Redis Key {source_key} 在复制期间由 {expected_kind:?} 变为 {final_kind:?}"
        )));
    }
    if final_ttl == -1 {
        if !redis
            .persist_key(&prepared.target, target_db, temp_key)
            .await?
        {
            return Err(DomainError::QueryFailed(
                "Redis 临时 Key 在恢复永久 TTL 前消失".into(),
            ));
        }
    } else if final_ttl >= 0 {
        let elapsed_ms: i64 = ttl_observed_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(i64::MAX);
        let remaining = final_ttl.saturating_sub(elapsed_ms);
        if remaining <= 0 {
            cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
            return Ok(CopyKeyOutcome::Skipped);
        }
        if !redis
            .set_ttl_ms(&prepared.target, target_db, temp_key, remaining)
            .await?
        {
            return Err(DomainError::QueryFailed(
                "Redis 临时 Key 在恢复 TTL 前消失".into(),
            ));
        }
    } else {
        return Err(DomainError::QueryFailed(format!(
            "Redis Key {source_key} 返回异常 PTTL：{final_ttl}"
        )));
    }

    if permit.cancellation_requested() {
        cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
        return Ok(CopyKeyOutcome::Cancelled);
    }
    let published = redis
        .rename_key_if_absent(&prepared.target, target_db, temp_key, target_key)
        .await?;
    if published {
        Ok(CopyKeyOutcome::Inserted)
    } else {
        cleanup_temp(service, &prepared.target, target_db, temp_key).await?;
        Ok(CopyKeyOutcome::Skipped)
    }
}

async fn cleanup_temp(
    service: &DataSyncService,
    target: &ConnectionConfig,
    db: u8,
    temp_key: &str,
) -> Result<()> {
    service
        .redis_service()
        .delete_key(target, db, temp_key)
        .await?;
    Ok(())
}

fn redis_value_kind(value: &RedisValue) -> Result<RedisType> {
    match value {
        RedisValue::Nil => Ok(RedisType::None),
        RedisValue::Text(_) | RedisValue::Bytes(_) => Ok(RedisType::String),
        RedisValue::List(_) => Ok(RedisType::List),
        RedisValue::Hash(_) => Ok(RedisType::Hash),
        RedisValue::Set(_) => Ok(RedisType::Set),
        RedisValue::ZSet(_) => Ok(RedisType::ZSet),
        RedisValue::Stream(_) => Ok(RedisType::Stream),
        RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) | RedisValue::Array(_) => {
            Err(DomainError::QueryFailed(
                "Redis 分页读取返回了不支持的值片段类型".into(),
            ))
        }
    }
}

fn page_has_payload(value: &RedisValue) -> bool {
    match value {
        // 空 String 仍是有效 Key，write_value_items 会用 SET 创建。
        RedisValue::Text(_) | RedisValue::Bytes(_) => true,
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            !values.is_empty()
        }
        RedisValue::Hash(values) => !values.is_empty(),
        RedisValue::ZSet(values) => !values.is_empty(),
        RedisValue::Stream(values) => !values.is_empty(),
        RedisValue::Nil | RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) => false,
    }
}

fn redis_value_bytes(value: &RedisValue) -> u64 {
    match value {
        RedisValue::Nil => 0,
        RedisValue::Text(value) => value.len() as u64,
        RedisValue::Bytes(value) => value.len() as u64,
        RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) => 8,
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            values.iter().fold(0u64, |total, value| {
                total.saturating_add(redis_value_bytes(value))
            })
        }
        RedisValue::Hash(values) => values.iter().fold(0u64, |total, (field, value)| {
            total
                .saturating_add(field.len() as u64)
                .saturating_add(redis_value_bytes(value))
        }),
        RedisValue::ZSet(values) => values.iter().fold(0u64, |total, (value, _)| {
            total
                .saturating_add(redis_value_bytes(value))
                .saturating_add(8)
        }),
        RedisValue::Stream(entries) => entries.iter().fold(0u64, |total, entry| {
            entry.fields.iter().fold(
                total.saturating_add(entry.id.len() as u64),
                |entry_total, (field, value)| {
                    entry_total
                        .saturating_add(field.len() as u64)
                        .saturating_add(value.len() as u64)
                },
            )
        }),
    }
}

/// Redis MATCH 使用 glob 语法；前缀选择必须逐字匹配，不能把用户输入当 glob。
pub(super) fn redis_literal_prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len().saturating_add(1));
    for character in prefix.chars() {
        if matches!(character, '*' | '?' | '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('*');
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_prefix_escapes_every_redis_glob_metacharacter() {
        assert_eq!(
            redis_literal_prefix_pattern(r"a*b?[c]\d"),
            r"a\*b\?\[c\]\\d*"
        );
        assert_eq!(redis_literal_prefix_pattern("普通:"), "普通:*");
    }

    #[test]
    fn value_byte_count_saturates_nested_values() {
        let value = RedisValue::Hash(vec![(
            "field".into(),
            RedisValue::List(vec![RedisValue::Text("value".into())]),
        )]);
        assert_eq!(redis_value_bytes(&value), 10);
    }

    #[test]
    fn page_kind_rejects_non_key_fragments() {
        assert_eq!(
            redis_value_kind(&RedisValue::Bytes(vec![1, 2])).expect("Bytes 是 String"),
            RedisType::String
        );
        assert!(redis_value_kind(&RedisValue::Int(1)).is_err());
        assert!(page_has_payload(&RedisValue::Text(String::new())));
        assert!(!page_has_payload(&RedisValue::Hash(Vec::new())));
    }

    #[test]
    fn temporary_ttl_is_long_but_finite() {
        assert_eq!(
            std::time::Duration::from_millis(REDIS_TEMP_TTL_MS as u64).as_secs(),
            86_400
        );
    }
}
