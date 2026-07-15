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

const HISTORY_MAX_KEEP: usize = 5000;
const MAX_HISTORY_RECORD_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_HISTORY_MUTATION_SCAN_BYTES: usize = 256 * 1024 * 1024;
const HISTORY_DELETE_BATCH: usize = 512;

fn storage_err(context: &str, error: impl std::fmt::Display) -> DomainError {
    DomainError::Storage(format!("{context}：{error}"))
}

fn decode_record(key: &str, value: &str) -> Result<QueryRecord> {
    bounded_json::ensure_len(
        value.len(),
        MAX_HISTORY_RECORD_JSON_BYTES,
        &format!("history 记录 {key}"),
    )?;
    serde_json::from_str(value)
        .map_err(|error| storage_err(&format!("反序列化 history 记录 {key} 失败"), error))
}

pub(crate) fn append(db: Arc<Database>, record: QueryRecord) -> Result<()> {
    let key = format!("{}_{}", record.executed_at.to_rfc3339(), record.id);
    let value = bounded_json::serialize(&record, MAX_HISTORY_RECORD_JSON_BYTES, "history 记录")?;

    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(HISTORY_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
        table
            .insert(key.as_str(), value.as_str())
            .map_err(|e| DomainError::Storage(format!("写入历史失败：{e}")))?;

        let len = usize::try_from(
            table
                .len()
                .map_err(|error| storage_err("读取 history 数量失败", error))?,
        )
        .map_err(|_| DomainError::Storage("history 数量超出当前平台可处理范围".into()))?;
        if len > HISTORY_MAX_KEEP {
            let mut remaining = len - HISTORY_MAX_KEEP;
            while remaining > 0 {
                let oldest_keys = collect_first_keys(
                    &table,
                    remaining.min(HISTORY_DELETE_BATCH),
                    "裁剪 history",
                )?;
                if oldest_keys.is_empty() {
                    return Err(DomainError::Storage(
                        "history 数量与实际记录不一致，无法安全裁剪".into(),
                    ));
                }
                remaining = remaining.saturating_sub(oldest_keys.len());
                for key in oldest_keys {
                    table
                        .remove(key.as_str())
                        .map_err(|error| storage_err("裁剪 history 失败", error))?;
                }
            }
        }
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
            table
                .remove(k.as_str())
                .map_err(|error| storage_err("删除 history 失败", error))?;
        }
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    Ok(())
}

pub(crate) fn clear(db: Arc<Database>, conn_filter: Option<ConnectionId>) -> Result<()> {
    let write_txn = db
        .begin_write()
        .map_err(|e| DomainError::Storage(format!("启动写事务失败：{e}")))?;
    {
        let mut table = write_txn
            .open_table(HISTORY_TABLE)
            .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;

        if let Some(target) = conn_filter {
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
                    to_remove.push(key.value().to_string());
                }
            }
            for k in to_remove {
                table
                    .remove(k.as_str())
                    .map_err(|error| storage_err("清空 history 失败", error))?;
            }
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
        }
    }
    write_txn
        .commit()
        .map_err(|e| DomainError::Storage(format!("提交事务失败：{e}")))?;
    info!("history cleared");
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

pub(crate) fn ensure_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    let _ = write_txn
        .open_table(HISTORY_TABLE)
        .map_err(|e| DomainError::Storage(format!("打开 history 表失败：{e}")))?;
    Ok(())
}
