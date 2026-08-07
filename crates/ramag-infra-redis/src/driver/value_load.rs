//! Redis 详情值的分类型、有界读取与内存统计。

use super::*;

pub(super) const DEFAULT_COLLECTION_LIMIT: usize = MAX_REDIS_COLLECTION_ITEMS;
/// 大集合在后台按固定批次读取，避免把全局 100 万条上限直接变成单次超大响应。
pub(super) const COLLECTION_FETCH_BATCH: usize = 5_000;
/// String 详情与其它结果共用 256 MiB 上限。
pub(super) const MAX_STRING_BYTES: u64 = MAX_REDIS_COLLECTION_BYTES as u64;

pub(super) async fn fetch_value_len(
    mgr: &mut ConnectionManager,
    key: &str,
    kind: RedisType,
) -> Result<Option<u64>> {
    let command = match kind {
        RedisType::List => "LLEN",
        RedisType::Hash => "HLEN",
        RedisType::Set => "SCARD",
        RedisType::ZSet => "ZCARD",
        RedisType::Stream => "XLEN",
        RedisType::String => "STRLEN",
        RedisType::None => return Ok(None),
    };
    let total: u64 = redis::cmd(command)
        .arg(key)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    Ok(Some(total))
}

pub(super) async fn fetch_string(
    mgr: &mut ConnectionManager,
    key: &str,
    total_bytes: u64,
) -> Result<(RedisValue, bool)> {
    let v: RV = redis::cmd("GETRANGE")
        .arg(key)
        .arg(0)
        .arg(MAX_STRING_BYTES.saturating_sub(1))
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "GETRANGE")?;
    let truncated = total_bytes > MAX_STRING_BYTES;
    Ok((decode_string_prefix(v, truncated), truncated))
}

pub(super) fn decode_string_prefix(value: RV, truncated: bool) -> RedisValue {
    let RV::BulkString(bytes) = value else {
        return decode_value(value);
    };
    match String::from_utf8(bytes) {
        Ok(text) => RedisValue::Text(text),
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_up_to);
            match String::from_utf8(bytes) {
                Ok(text) => RedisValue::Text(text),
                Err(error) => RedisValue::Bytes(error.into_bytes()),
            }
        }
        Err(error) => RedisValue::Bytes(error.into_bytes()),
    }
}

pub(super) async fn fetch_list(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    let mut elems = Vec::with_capacity(limit.min(COLLECTION_FETCH_BATCH));
    let mut offset = 0usize;
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    while elems.len() < limit {
        let count = limit
            .saturating_sub(elems.len())
            .min(COLLECTION_FETCH_BATCH);
        let end = offset.saturating_add(count).saturating_sub(1);
        let v: RV = redis::cmd("LRANGE")
            .arg(key)
            .arg(offset)
            .arg(end)
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        ensure_response_budget(&v, "LRANGE")?;
        let batch = match v {
            RV::Array(values) => values,
            RV::Nil => Vec::new(),
            other => {
                return Err(DomainError::QueryFailed(format!(
                    "LRANGE 应答非数组：{other:?}"
                )));
            }
        };
        let batch_len = batch.len();
        for value in batch.into_iter().map(decode_value) {
            let item_bytes = redis_value_retained_bytes(&value);
            let Some(next_bytes) =
                reserve_retained_bytes(retained_bytes, item_bytes, MAX_REDIS_COLLECTION_BYTES)
            else {
                byte_limited = true;
                break;
            };
            retained_bytes = next_bytes;
            elems.push(value);
        }
        if batch_len < count || byte_limited {
            break;
        }
        offset = offset.saturating_add(batch_len);
    }
    Ok((RedisValue::List(elems), byte_limited))
}

pub(super) async fn fetch_hash(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    // COUNT 只是 hint，必须续扫游标，不能把第一批误当成完整结果。
    let mut cursor = 0u64;
    let mut pairs = Vec::with_capacity(limit.min(COLLECTION_FETCH_BATCH));
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    loop {
        let v: RV = redis::cmd("HSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(
                limit
                    .saturating_sub(pairs.len())
                    .clamp(1, COLLECTION_FETCH_BATCH),
            )
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        ensure_response_budget(&v, "HSCAN")?;
        let (next, payload) = scan_parts(v, "HSCAN")?;
        let RedisValue::Hash(batch) = decode_hash_pairs(payload)? else {
            return Err(DomainError::QueryFailed("HSCAN 解码结果类型异常".into()));
        };
        for (field, value) in batch.into_iter().take(limit.saturating_sub(pairs.len())) {
            let item_bytes = std::mem::size_of::<(String, RedisValue)>()
                .saturating_add(field.len())
                .saturating_add(redis_value_retained_bytes(&value));
            let Some(next_bytes) =
                reserve_retained_bytes(retained_bytes, item_bytes, MAX_REDIS_COLLECTION_BYTES)
            else {
                byte_limited = true;
                break;
            };
            retained_bytes = next_bytes;
            pairs.push((field, value));
        }
        cursor = next;
        if cursor == 0 || pairs.len() >= limit || byte_limited {
            break;
        }
    }
    Ok((RedisValue::Hash(pairs), byte_limited))
}

pub(super) async fn fetch_set(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    let mut cursor = 0u64;
    let mut elems = Vec::with_capacity(limit.min(COLLECTION_FETCH_BATCH));
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    loop {
        let v: RV = redis::cmd("SSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(
                limit
                    .saturating_sub(elems.len())
                    .clamp(1, COLLECTION_FETCH_BATCH),
            )
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        ensure_response_budget(&v, "SSCAN")?;
        let (next, payload) = scan_parts(v, "SSCAN")?;
        match payload {
            RV::Array(a) => {
                for value in a
                    .into_iter()
                    .map(decode_value)
                    .take(limit.saturating_sub(elems.len()))
                {
                    let item_bytes = redis_value_retained_bytes(&value);
                    let Some(next_bytes) = reserve_retained_bytes(
                        retained_bytes,
                        item_bytes,
                        MAX_REDIS_COLLECTION_BYTES,
                    ) else {
                        byte_limited = true;
                        break;
                    };
                    retained_bytes = next_bytes;
                    elems.push(value);
                }
            }
            RV::Nil => {}
            other => {
                return Err(DomainError::QueryFailed(format!(
                    "SSCAN 应答非数组：{other:?}"
                )));
            }
        }
        cursor = next;
        if cursor == 0 || elems.len() >= limit || byte_limited {
            break;
        }
    }
    Ok((RedisValue::Set(elems), byte_limited))
}

pub(super) async fn fetch_zset(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    let mut pairs = Vec::with_capacity(limit.min(COLLECTION_FETCH_BATCH));
    let mut offset = 0usize;
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    while pairs.len() < limit {
        let count = limit
            .saturating_sub(pairs.len())
            .min(COLLECTION_FETCH_BATCH);
        let end = offset.saturating_add(count).saturating_sub(1);
        let v: RV = redis::cmd("ZRANGE")
            .arg(key)
            .arg(offset)
            .arg(end)
            .arg("WITHSCORES")
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        ensure_response_budget(&v, "ZRANGE")?;
        let batch = match decode_zset_with_scores(v)? {
            RedisValue::ZSet(values) => values,
            RedisValue::Nil => Vec::new(),
            other => {
                return Err(DomainError::QueryFailed(format!(
                    "ZRANGE 解码结果类型异常：{}",
                    other.display_preview(32)
                )));
            }
        };
        let batch_len = batch.len();
        for (value, score) in batch {
            let item_bytes = std::mem::size_of::<(RedisValue, f64)>()
                .saturating_add(redis_value_retained_bytes(&value));
            let Some(next_bytes) =
                reserve_retained_bytes(retained_bytes, item_bytes, MAX_REDIS_COLLECTION_BYTES)
            else {
                byte_limited = true;
                break;
            };
            retained_bytes = next_bytes;
            pairs.push((value, score));
        }
        if batch_len < count || byte_limited {
            break;
        }
        offset = offset.saturating_add(batch_len);
    }
    Ok((RedisValue::ZSet(pairs), byte_limited))
}

pub(super) async fn fetch_stream(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    let mut entries = Vec::with_capacity(limit.min(COLLECTION_FETCH_BATCH));
    let mut start = "-".to_string();
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    while entries.len() < limit {
        let count = limit
            .saturating_sub(entries.len())
            .min(COLLECTION_FETCH_BATCH);
        let v: RV = redis::cmd("XRANGE")
            .arg(key)
            .arg(&start)
            .arg("+")
            .arg("COUNT")
            .arg(count)
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        ensure_response_budget(&v, "XRANGE")?;
        let batch = match decode_stream_entries(v)? {
            RedisValue::Stream(values) => values,
            RedisValue::Nil => Vec::new(),
            other => {
                return Err(DomainError::QueryFailed(format!(
                    "XRANGE 解码结果类型异常：{}",
                    other.display_preview(32)
                )));
            }
        };
        let batch_len = batch.len();
        let last_id = batch.last().map(|entry| entry.id.clone());
        for entry in batch {
            let item_bytes = stream_entry_retained_bytes(&entry);
            let Some(next_bytes) =
                reserve_retained_bytes(retained_bytes, item_bytes, MAX_REDIS_COLLECTION_BYTES)
            else {
                byte_limited = true;
                break;
            };
            retained_bytes = next_bytes;
            entries.push(entry);
        }
        if batch_len < count || byte_limited {
            break;
        }
        let Some(last_id) = last_id else {
            break;
        };
        // 开区间避免下一批重复上一批最后一条（Redis >= 6.2）。
        start = format!("({last_id}");
    }
    Ok((RedisValue::Stream(entries), byte_limited))
}

pub(super) fn stream_entry_retained_bytes(entry: &StreamEntry) -> usize {
    entry.fields.iter().fold(
        std::mem::size_of::<StreamEntry>().saturating_add(entry.id.len()),
        |total, (field, value)| {
            total
                .saturating_add(std::mem::size_of::<(String, String)>())
                .saturating_add(field.len())
                .saturating_add(value.len())
        },
    )
}

pub(super) fn reserve_retained_bytes(current: usize, added: usize, limit: usize) -> Option<usize> {
    current.checked_add(added).filter(|next| *next <= limit)
}

pub(super) fn redis_value_retained_bytes(value: &RedisValue) -> usize {
    let dynamic = match value {
        RedisValue::Nil | RedisValue::Int(_) | RedisValue::Float(_) | RedisValue::Bool(_) => 0,
        RedisValue::Text(text) => text.len(),
        RedisValue::Bytes(bytes) => bytes.len(),
        RedisValue::List(values) | RedisValue::Set(values) | RedisValue::Array(values) => {
            values.iter().fold(0usize, |total, value| {
                total.saturating_add(redis_value_retained_bytes(value))
            })
        }
        RedisValue::Hash(values) => values.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.len())
                .saturating_add(redis_value_retained_bytes(value))
        }),
        RedisValue::ZSet(values) => values.iter().fold(0usize, |total, (value, _)| {
            total.saturating_add(redis_value_retained_bytes(value))
        }),
        RedisValue::Stream(entries) => entries.iter().fold(0usize, |total, entry| {
            entry.fields.iter().fold(
                total.saturating_add(entry.id.len()),
                |entry_total, (key, value)| {
                    entry_total
                        .saturating_add(key.len())
                        .saturating_add(value.len())
                },
            )
        }),
    };
    std::mem::size_of::<RedisValue>().saturating_add(dynamic)
}
