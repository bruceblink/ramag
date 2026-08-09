//! 查询历史 CRUD。复合 key=`{rfc3339}_{id}`（时间字典序 + 防毫秒撞）；超 HISTORY_MAX_KEEP 删最早

use std::sync::Arc;

use redb::{
    Database, ReadableDatabase as _, ReadableTable, ReadableTableMetadata as _, TableDefinition,
};
use tracing::{debug, info};

use ramag_domain::entities::{ConnectionId, QueryHistoryPage, QueryRecord, QueryRecordId};
use ramag_domain::error::{DomainError, Result};

use crate::repos::bounded_json;

pub(crate) const HISTORY_TABLE: TableDefinition<&str, &str> = TableDefinition::new("query_history");
pub(crate) const HISTORY_META_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("query_history_meta");

const HISTORY_MAX_KEEP: usize = 5000;
const MAX_HISTORY_RECORD_JSON_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_HISTORY_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HISTORY_MUTATION_SCAN_BYTES: usize = 256 * 1024 * 1024;
const HISTORY_DELETE_BATCH: usize = 512;
const HISTORY_META_COUNT_KEY: &str = "record_count";
const HISTORY_META_BYTES_KEY: &str = "value_bytes";

fn storage_err(context: &str, error: impl std::fmt::Display) -> DomainError {
    DomainError::Storage(format!("{context}：{error}"))
}

fn decode_record(key: &str, value: &str) -> Result<QueryRecord> {
    bounded_json::ensure_len(
        value.len(),
        MAX_HISTORY_RECORD_JSON_BYTES,
        &format!("history 记录 {key}"),
    )?;
    let mut record: QueryRecord = serde_json::from_str(value)
        .map_err(|error| storage_err(&format!("反序列化 history 记录 {key} 失败"), error))?;
    record.enforce_limits();
    record
        .validate()
        .map_err(|error| DomainError::Storage(format!("history 记录 {key} 无效：{error}")))?;
    Ok(record)
}

pub(crate) fn append(db: Arc<Database>, record: QueryRecord) -> Result<()> {
    append_with_budget(db, record, HISTORY_MAX_KEEP, MAX_HISTORY_TOTAL_BYTES)
}

fn append_with_budget(
    db: Arc<Database>,
    mut record: QueryRecord,
    max_records: usize,
    max_total_bytes: u64,
) -> Result<()> {
    record.enforce_limits();
    record
        .validate()
        .map_err(|error| DomainError::Storage(format!("history 记录无效：{error}")))?;
    let key = format!("{}_{}", record.executed_at.to_rfc3339(), record.id);
    let value = bounded_json::serialize(&record, MAX_HISTORY_RECORD_JSON_BYTES, "history 记录")?;

    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(HISTORY_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
        let mut meta = write_txn
            .open_table(HISTORY_META_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 元数据表失败：{e}")))?;
        let (mut record_count, mut total_bytes) = load_history_stats(&table, &meta)?;
        let replaced = table
            .insert(key.as_str(), value.as_str())
            .map_err(|e| DomainError::Storage(format!("写入历史失败：{e}")))?;
        let replaced_bytes = replaced
            .as_ref()
            .map(|old| value_len_u64(old.value().len()))
            .transpose()?;
        drop(replaced);
        if replaced_bytes.is_none() {
            record_count = record_count
                .checked_add(1)
                .ok_or_else(|| DomainError::Storage("history 数量溢出".into()))?;
        }
        total_bytes = total_bytes
            .saturating_sub(replaced_bytes.unwrap_or(0))
            .checked_add(value_len_u64(value.len())?)
            .ok_or_else(|| DomainError::Storage("history 总大小溢出".into()))?;

        while history_over_budget(record_count, total_bytes, max_records, max_total_bytes) {
            let oldest = collect_first_entries(&table, HISTORY_DELETE_BATCH, "裁剪 history")?;
            if oldest.is_empty() {
                return Err(DomainError::Storage(
                    "history 元数据与实际记录不一致，无法安全裁剪".into(),
                ));
            }
            let mut removed_any = false;
            for (oldest_key, expected_bytes) in oldest {
                if !history_over_budget(record_count, total_bytes, max_records, max_total_bytes) {
                    break;
                }
                let removed = table
                    .remove(oldest_key.as_str())
                    .map_err(|error| storage_err("裁剪 history 失败", error))?
                    .ok_or_else(|| DomainError::Storage("裁剪 history 时记录已消失".into()))?;
                let removed_bytes = value_len_u64(removed.value().len())?;
                drop(removed);
                record_count = record_count.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(removed_bytes);
                removed_any = true;
                if removed_bytes != expected_bytes {
                    return Err(DomainError::Storage(
                        "裁剪 history 时记录大小发生变化，事务已取消".into(),
                    ));
                }
            }
            if !removed_any {
                return Err(DomainError::Storage("无法推进 history 裁剪".into()));
            }
        }
        persist_history_stats(&mut meta, record_count, total_bytes)?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    debug!(record_id = %record.id, "history appended");
    Ok(())
}

pub(crate) fn list(
    db: Arc<Database>,
    conn_filter: Option<ConnectionId>,
    limit: usize,
) -> Result<Vec<QueryRecord>> {
    Ok(list_bounded(db, conn_filter, limit, u64::MAX)?.records)
}

pub(crate) fn list_bounded(
    db: Arc<Database>,
    conn_filter: Option<ConnectionId>,
    limit: usize,
    max_inline_bytes: u64,
) -> Result<QueryHistoryPage> {
    let read_txn = db
        .begin_read()
        .map_err(|e| DomainError::Storage(format!("启动读事务失败：{e}")))?;
    let table = read_txn
        .open_table(HISTORY_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;

    if limit == 0 || max_inline_bytes == 0 {
        return Ok(QueryHistoryPage {
            records: Vec::new(),
            truncated: false,
        });
    }
    let mut records = Vec::with_capacity(limit.min(HISTORY_MAX_KEEP));
    let mut total_inline_bytes = 0u64;
    let mut truncated = false;
    for entry in table
        .iter()
        .map_err(|e| DomainError::Storage(e.to_string()))?
        .rev()
    {
        let (key, value) = entry.map_err(|error| storage_err("读取 history 记录失败", error))?;
        let rec = decode_record(key.value(), value.value())?;
        if let Some(ref filter_id) = conn_filter
            && rec.connection_id != *filter_id
        {
            continue;
        }
        let next_total = total_inline_bytes.saturating_add(rec.inline_payload_bytes());
        if records.len() >= limit || (!records.is_empty() && next_total > max_inline_bytes) {
            truncated = true;
            break;
        }
        total_inline_bytes = next_total;
        records.push(rec);
    }
    Ok(QueryHistoryPage { records, truncated })
}

pub(crate) fn delete(db: Arc<Database>, id: QueryRecordId) -> Result<()> {
    let target_id = id.0.to_string();

    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(HISTORY_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
        let mut meta = write_txn
            .open_table(HISTORY_META_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 元数据表失败：{e}")))?;
        let (mut record_count, mut total_bytes) = load_history_stats(&table, &meta)?;
        // QueryRecordId 唯一；找到目标 key 后立即停止，不保留整表 guard / key。
        let mut target_key = None;
        for entry in table
            .iter()
            .map_err(|error| storage_err("遍历 history 失败", error))?
        {
            let (key, _) = entry.map_err(|error| storage_err("读取 history 记录失败", error))?;
            if key.value().ends_with(&target_id) {
                target_key = Some(key.value().to_string());
                break;
            }
        }
        if let Some(k) = target_key {
            let removed = table
                .remove(k.as_str())
                .map_err(|error| storage_err("删除 history 失败", error))?;
            if let Some(removed) = removed {
                let removed_bytes = value_len_u64(removed.value().len())?;
                drop(removed);
                record_count = record_count.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(removed_bytes);
            }
        }
        persist_history_stats(&mut meta, record_count, total_bytes)?;
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    Ok(())
}

pub(crate) fn clear(db: Arc<Database>, conn_filter: Option<ConnectionId>) -> Result<()> {
    let scope = conn_filter
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "all".into());
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(HISTORY_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
        let mut meta = write_txn
            .open_table(HISTORY_META_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 元数据表失败：{e}")))?;

        if let Some(target) = conn_filter {
            let (mut record_count, mut total_bytes) = load_history_stats(&table, &meta)?;
            let mut to_remove = Vec::new();
            let mut scanned_count = 0usize;
            let mut scanned_bytes = 0usize;
            for entry in table
                .iter()
                .map_err(|error| storage_err("遍历 history 失败", error))?
            {
                let (key, value) =
                    entry.map_err(|error| storage_err("读取 history 记录失败", error))?;
                (scanned_count, scanned_bytes) = bounded_json::next_collection_budget(
                    scanned_count,
                    scanned_bytes,
                    value.value().len(),
                    HISTORY_MAX_KEEP,
                    MAX_HISTORY_MUTATION_SCAN_BYTES,
                    "查询历史",
                )
                .map_err(|error| {
                    DomainError::Storage(format!(
                        "按连接清空前发现历史表异常：{}；请使用全量清空恢复",
                        error.message()
                    ))
                })?;
                let rec = decode_record(key.value(), value.value())?;
                if rec.connection_id == target {
                    to_remove.push((key.value().to_string(), value_len_u64(value.value().len())?));
                }
            }
            for (k, expected_bytes) in to_remove {
                let removed = table
                    .remove(k.as_str())
                    .map_err(|error| storage_err("清空 history 失败", error))?
                    .ok_or_else(|| DomainError::Storage("清空 history 时记录已消失".into()))?;
                let removed_bytes = value_len_u64(removed.value().len())?;
                drop(removed);
                if removed_bytes != expected_bytes {
                    return Err(DomainError::Storage(
                        "清空 history 时记录大小发生变化，事务已取消".into(),
                    ));
                }
                record_count = record_count.saturating_sub(1);
                total_bytes = total_bytes.saturating_sub(removed_bytes);
            }
            persist_history_stats(&mut meta, record_count, total_bytes)?;
        } else {
            // 全量清空是损坏数据恢复入口：固定批次收集 key，内存不随异常表体量增长。
            loop {
                let keys = collect_first_keys(&table, HISTORY_DELETE_BATCH, "清空 history")?;
                if keys.is_empty() {
                    break;
                }
                for key in keys {
                    table
                        .remove(key.as_str())
                        .map_err(|error| storage_err("清空 history 失败", error))?;
                }
            }
            persist_history_stats(&mut meta, 0, 0)?;
        }
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    info!(operation = "query_history_clear", scope = %scope, "history cleared");
    Ok(())
}

fn collect_first_keys(
    table: &redb::Table<'_, &str, &str>,
    limit: usize,
    context: &str,
) -> Result<Vec<String>> {
    let mut keys = Vec::with_capacity(limit);
    for entry in table
        .iter()
        .map_err(|error| storage_err(&format!("{context}时遍历失败"), error))?
        .take(limit)
    {
        let (key, _) =
            entry.map_err(|error| storage_err(&format!("{context}时读取记录失败"), error))?;
        keys.push(key.value().to_string());
    }
    Ok(keys)
}

fn collect_first_entries(
    table: &redb::Table<'_, &str, &str>,
    limit: usize,
    context: &str,
) -> Result<Vec<(String, u64)>> {
    let mut entries = Vec::with_capacity(limit);
    for entry in table
        .iter()
        .map_err(|error| storage_err(&format!("{context}时遍历失败"), error))?
        .take(limit)
    {
        let (key, value) =
            entry.map_err(|error| storage_err(&format!("{context}时读取记录失败"), error))?;
        entries.push((key.value().to_string(), value_len_u64(value.value().len())?));
    }
    Ok(entries)
}

fn load_history_stats(
    table: &redb::Table<'_, &str, &str>,
    meta: &redb::Table<'_, &str, u64>,
) -> Result<(usize, u64)> {
    let actual_count = usize::try_from(
        table
            .len()
            .map_err(|error| storage_err("读取 history 数量失败", error))?,
    )
    .map_err(|_| DomainError::Storage("history 数量超出当前平台可处理范围".into()))?;
    let stored_count = meta
        .get(HISTORY_META_COUNT_KEY)
        .map_err(|error| storage_err("读取 history 计数元数据失败", error))?
        .map(|value| value.value());
    let stored_bytes = meta
        .get(HISTORY_META_BYTES_KEY)
        .map_err(|error| storage_err("读取 history 大小元数据失败", error))?
        .map(|value| value.value());
    if stored_count == Some(actual_count as u64)
        && let Some(stored_bytes) = stored_bytes
    {
        return Ok((actual_count, stored_bytes));
    }

    let mut total_bytes = 0u64;
    for entry in table
        .iter()
        .map_err(|error| storage_err("重建 history 元数据时遍历失败", error))?
    {
        let (key, value) =
            entry.map_err(|error| storage_err("重建 history 元数据时读取失败", error))?;
        bounded_json::ensure_len(
            value.value().len(),
            MAX_HISTORY_RECORD_JSON_BYTES,
            &format!("history 记录 {}", key.value()),
        )?;
        total_bytes = total_bytes
            .checked_add(value_len_u64(value.value().len())?)
            .ok_or_else(|| DomainError::Storage("history 总大小溢出".into()))?;
    }
    Ok((actual_count, total_bytes))
}

fn persist_history_stats(
    meta: &mut redb::Table<'_, &str, u64>,
    record_count: usize,
    total_bytes: u64,
) -> Result<()> {
    let record_count = u64::try_from(record_count)
        .map_err(|_| DomainError::Storage("history 数量超出 u64 范围".into()))?;
    meta.insert(HISTORY_META_COUNT_KEY, record_count)
        .map_err(|error| storage_err("写入 history 计数元数据失败", error))?;
    meta.insert(HISTORY_META_BYTES_KEY, total_bytes)
        .map_err(|error| storage_err("写入 history 大小元数据失败", error))?;
    Ok(())
}

fn history_over_budget(
    record_count: usize,
    total_bytes: u64,
    max_records: usize,
    max_total_bytes: u64,
) -> bool {
    record_count > max_records || total_bytes > max_total_bytes
}

fn value_len_u64(value_len: usize) -> Result<u64> {
    u64::try_from(value_len)
        .map_err(|_| DomainError::Storage("history 记录大小超出 u64 范围".into()))
}

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(HISTORY_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
    let _ = write_txn
        .open_table(HISTORY_META_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 history 元数据表失败：{e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use ramag_domain::entities::{
        ConnectionId, MAX_QUERY_HISTORY_ERROR_BYTES, MAX_QUERY_HISTORY_SQL_BYTES,
    };
    use tempfile::TempDir;

    #[test]
    fn bounded_history_always_fits_serialization_budget() {
        let sql = "\0".repeat(MAX_QUERY_HISTORY_SQL_BYTES + 1);
        let error = "\0".repeat(MAX_QUERY_HISTORY_ERROR_BYTES + 1);
        let record = QueryRecord::new_failed(ConnectionId::new(), "local", &sql, &error);

        assert!(record.sql_truncated);
        assert!(record.error_truncated);
        assert!(
            bounded_json::serialize(&record, MAX_HISTORY_RECORD_JSON_BYTES, "history 记录").is_ok()
        );
    }

    #[test]
    fn history_budget_checks_count_and_total_bytes() {
        assert!(!history_over_budget(
            HISTORY_MAX_KEEP,
            MAX_HISTORY_TOTAL_BYTES,
            HISTORY_MAX_KEEP,
            MAX_HISTORY_TOTAL_BYTES,
        ));
        assert!(history_over_budget(
            HISTORY_MAX_KEEP + 1,
            MAX_HISTORY_TOTAL_BYTES,
            HISTORY_MAX_KEEP,
            MAX_HISTORY_TOTAL_BYTES,
        ));
        assert!(history_over_budget(
            HISTORY_MAX_KEEP,
            MAX_HISTORY_TOTAL_BYTES + 1,
            HISTORY_MAX_KEEP,
            MAX_HISTORY_TOTAL_BYTES,
        ));
    }

    #[test]
    fn append_prunes_oldest_record_when_total_budget_is_exceeded()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = TempDir::new()?;
        let db = Database::create(temp.path().join("history.redb"))?;
        let txn = db.begin_write()?;
        ensure_table(&txn)?;
        txn.commit()?;
        let db = Arc::new(db);
        let connection_id = ConnectionId::new();
        let mut first =
            QueryRecord::new_success(connection_id.clone(), "local", "SELECT 'first'", 1, 1);
        first.executed_at = Utc::now() - Duration::seconds(1);
        let mut second = QueryRecord::new_success(connection_id, "local", "SELECT 'second'", 1, 1);
        second.executed_at = Utc::now();
        let first_bytes = value_len_u64(
            bounded_json::serialize(&first, MAX_HISTORY_RECORD_JSON_BYTES, "history 记录")?.len(),
        )?;
        let second_bytes = value_len_u64(
            bounded_json::serialize(&second, MAX_HISTORY_RECORD_JSON_BYTES, "history 记录")?.len(),
        )?;
        let budget = first_bytes + second_bytes - 1;

        append_with_budget(db.clone(), first, HISTORY_MAX_KEEP, budget)?;
        append_with_budget(db.clone(), second.clone(), HISTORY_MAX_KEEP, budget)?;

        let records = list(db, None, 10)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, second.id);
        Ok(())
    }
}
