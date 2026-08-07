//! 导出 / 导入的全量分段读写（`KvDriver::read_value_page` / `write_value_items` 实现体）。
//! 与 driver.rs 的受限预览路径（`get_value_limited`）互不影响：这里逐页覆盖完整内容，
//! 二进制成员经 `RedisValue::Bytes` 保真；仅实体无法表达的条目（二进制 hash field /
//! stream field）跳过并计数
mod value_ops;

use value_ops::*;

use std::borrow::Cow;

use ramag_domain::entities::{
    MAX_REDIS_COMMAND_ARG_BYTES, RedisType, RedisValue, RedisValuePage, StreamEntry,
    TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, ValuePageCursor,
};
use ramag_domain::error::{DomainError, Result};
use redis::Value as RV;
use redis::aio::ConnectionManager;

use crate::driver::{ensure_response_budget, scan_parts};
use crate::errors::map_redis_error;
use crate::value::{decode_value, decode_zset_with_scores};

/// 单页条目上限，与传输和响应预算一致。
pub const MAX_PAGE_ITEMS: u32 = TRANSFER_BATCH_ITEMS as u32;
/// String 类型单页读取字节数
const STRING_PAGE_BYTES: u64 = 1024 * 1024;
/// 单条写命令的成员数与字节预算，超过即分批发送
const WRITE_CHUNK_MEMBERS: usize = TRANSFER_BATCH_ITEMS;
const WRITE_CHUNK_BYTES: usize = TRANSFER_BATCH_BYTES;

pub(crate) fn validate_page_items(max_items: u32) -> Result<()> {
    if !(1..=MAX_PAGE_ITEMS).contains(&max_items) {
        return Err(DomainError::InvalidConfig(format!(
            "Redis 分页条目数必须在 1 - {MAX_PAGE_ITEMS} 之间"
        )));
    }
    Ok(())
}

pub(crate) async fn read_page(
    mgr: &mut ConnectionManager,
    key: &str,
    kind: Option<RedisType>,
    cursor: ValuePageCursor,
    max_items: u32,
) -> Result<RedisValuePage> {
    // 首页未传类型：单次往返管道化 TYPE + PTTL，省掉逐 key 的独立探测
    let (kind, ttl_ms) = match kind {
        Some(kind) => (kind, None),
        None => {
            if !matches!(cursor, ValuePageCursor::Start) {
                return Err(DomainError::InvalidConfig(
                    "Redis 续读分页必须携带 key 类型".into(),
                ));
            }
            let (type_text, ttl): (String, i64) = redis::pipe()
                .cmd("TYPE")
                .arg(key)
                .cmd("PTTL")
                .arg(key)
                .query_async(&mut *mgr)
                .await
                .map_err(map_redis_error)?;
            (supported_key_type(&type_text, key)?, Some(ttl))
        }
    };
    let mut page = match kind {
        RedisType::None => done_page(RedisValue::Nil),
        RedisType::String => read_string_page(mgr, key, offset_cursor(&cursor, "String")?).await?,
        RedisType::List => {
            read_list_page(mgr, key, offset_cursor(&cursor, "List")?, max_items).await?
        }
        RedisType::Hash => {
            read_hash_page(mgr, key, scan_cursor(&cursor, "Hash")?, max_items).await?
        }
        RedisType::Set => read_set_page(mgr, key, scan_cursor(&cursor, "Set")?, max_items).await?,
        RedisType::ZSet => {
            read_zset_page(mgr, key, offset_cursor(&cursor, "ZSet")?, max_items).await?
        }
        RedisType::Stream => read_stream_page(mgr, key, &cursor, max_items).await?,
    };
    page.ttl_ms = ttl_ms;
    Ok(page)
}

fn supported_key_type(type_text: &str, key: &str) -> Result<RedisType> {
    let parsed = RedisType::parse(type_text);
    if parsed == RedisType::None && type_text != "none" {
        return Err(DomainError::InvalidConfig(format!(
            "Redis Key {key} 的类型 {type_text} 不受当前同步版本支持"
        )));
    }
    Ok(parsed)
}

pub(crate) async fn write_items(
    mgr: &mut ConnectionManager,
    key: &str,
    items: &RedisValue,
) -> Result<u64> {
    match items {
        RedisValue::Nil => Ok(0),
        RedisValue::Text(text) => append_chunk(mgr, key, text.as_bytes()).await,
        RedisValue::Bytes(bytes) => append_chunk(mgr, key, bytes).await,
        RedisValue::List(members) => write_members(mgr, key, "RPUSH", members).await,
        RedisValue::Set(members) => write_members(mgr, key, "SADD", members).await,
        RedisValue::Hash(pairs) => write_hash(mgr, key, pairs).await,
        RedisValue::ZSet(pairs) => write_zset(mgr, key, pairs).await,
        RedisValue::Stream(entries) => write_stream(mgr, key, entries).await,
        other => Err(DomainError::InvalidConfig(format!(
            "该片段类型不支持导入写入：{}",
            other.display_preview(32)
        ))),
    }
}

fn done_page(items: RedisValue) -> RedisValuePage {
    RedisValuePage {
        items,
        next: None,
        skipped: 0,
        ttl_ms: None,
    }
}

fn offset_cursor(cursor: &ValuePageCursor, kind: &str) -> Result<u64> {
    match cursor {
        ValuePageCursor::Start => Ok(0),
        ValuePageCursor::Offset(offset) => Ok(*offset),
        other => Err(cursor_mismatch(kind, other)),
    }
}

fn scan_cursor(cursor: &ValuePageCursor, kind: &str) -> Result<u64> {
    match cursor {
        ValuePageCursor::Start => Ok(0),
        ValuePageCursor::Scan(cursor) => Ok(*cursor),
        other => Err(cursor_mismatch(kind, other)),
    }
}

fn cursor_mismatch(kind: &str, cursor: &ValuePageCursor) -> DomainError {
    DomainError::InvalidConfig(format!("Redis {kind} 类型不支持游标 {cursor:?}"))
}

async fn read_string_page(
    mgr: &mut ConnectionManager,
    key: &str,
    offset: u64,
) -> Result<RedisValuePage> {
    let total: u64 = redis::cmd("STRLEN")
        .arg(key)
        .query_async(&mut *mgr)
        .await
        .map_err(map_redis_error)?;
    let end = offset.saturating_add(STRING_PAGE_BYTES).saturating_sub(1);
    let v: RV = redis::cmd("GETRANGE")
        .arg(key)
        .arg(offset)
        .arg(end)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "GETRANGE")?;
    let chunk = match v {
        RV::BulkString(bytes) => bytes,
        RV::SimpleString(text) => text.into_bytes(),
        RV::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "GETRANGE 应答类型异常：{other:?}"
            )));
        }
    };
    let read = chunk.len() as u64;
    let next_offset = offset.saturating_add(read);
    let next = (read > 0 && next_offset < total).then_some(ValuePageCursor::Offset(next_offset));
    let items = match String::from_utf8(chunk) {
        Ok(text) => RedisValue::Text(text),
        Err(error) => RedisValue::Bytes(error.into_bytes()),
    };
    Ok(RedisValuePage {
        items,
        next,
        skipped: 0,
        ttl_ms: None,
    })
}

async fn read_list_page(
    mgr: &mut ConnectionManager,
    key: &str,
    offset: u64,
    max_items: u32,
) -> Result<RedisValuePage> {
    let end = offset
        .saturating_add(u64::from(max_items))
        .saturating_sub(1);
    let v: RV = redis::cmd("LRANGE")
        .arg(key)
        .arg(offset)
        .arg(end)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "LRANGE")?;
    let elems: Vec<RedisValue> = match v {
        RV::Array(a) => a.into_iter().map(decode_value).collect(),
        RV::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "LRANGE 应答非数组：{other:?}"
            )));
        }
    };
    let n = elems.len();
    let next =
        (n as u32 == max_items).then(|| ValuePageCursor::Offset(offset.saturating_add(n as u64)));
    Ok(RedisValuePage {
        items: RedisValue::List(elems),
        next,
        skipped: 0,
        ttl_ms: None,
    })
}

async fn read_hash_page(
    mgr: &mut ConnectionManager,
    key: &str,
    cursor: u64,
    max_items: u32,
) -> Result<RedisValuePage> {
    let v: RV = redis::cmd("HSCAN")
        .arg(key)
        .arg(cursor)
        .arg("COUNT")
        .arg(max_items)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "HSCAN")?;
    let (next_cursor, payload) = scan_parts(v, "HSCAN")?;
    let (pairs, skipped) = strict_hash_pairs(payload)?;
    let next = (next_cursor != 0).then_some(ValuePageCursor::Scan(next_cursor));
    Ok(RedisValuePage {
        items: RedisValue::Hash(pairs),
        next,
        skipped,
        ttl_ms: None,
    })
}

async fn read_set_page(
    mgr: &mut ConnectionManager,
    key: &str,
    cursor: u64,
    max_items: u32,
) -> Result<RedisValuePage> {
    let v: RV = redis::cmd("SSCAN")
        .arg(key)
        .arg(cursor)
        .arg("COUNT")
        .arg(max_items)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "SSCAN")?;
    let (next_cursor, payload) = scan_parts(v, "SSCAN")?;
    let elems: Vec<RedisValue> = match payload {
        RV::Array(a) => a.into_iter().map(decode_value).collect(),
        RV::Set(a) => a.into_iter().map(decode_value).collect(),
        RV::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SSCAN 应答非数组：{other:?}"
            )));
        }
    };
    let next = (next_cursor != 0).then_some(ValuePageCursor::Scan(next_cursor));
    Ok(RedisValuePage {
        items: RedisValue::Set(elems),
        next,
        skipped: 0,
        ttl_ms: None,
    })
}

async fn read_zset_page(
    mgr: &mut ConnectionManager,
    key: &str,
    offset: u64,
    max_items: u32,
) -> Result<RedisValuePage> {
    let end = offset
        .saturating_add(u64::from(max_items))
        .saturating_sub(1);
    let v: RV = redis::cmd("ZRANGE")
        .arg(key)
        .arg(offset)
        .arg(end)
        .arg("WITHSCORES")
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "ZRANGE")?;
    let pairs = match decode_zset_with_scores(v)? {
        RedisValue::ZSet(pairs) => pairs,
        RedisValue::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "ZRANGE 解码结果类型异常：{}",
                other.display_preview(32)
            )));
        }
    };
    let n = pairs.len();
    let next =
        (n as u32 == max_items).then(|| ValuePageCursor::Offset(offset.saturating_add(n as u64)));
    Ok(RedisValuePage {
        items: RedisValue::ZSet(pairs),
        next,
        skipped: 0,
        ttl_ms: None,
    })
}

async fn read_stream_page(
    mgr: &mut ConnectionManager,
    key: &str,
    cursor: &ValuePageCursor,
    max_items: u32,
) -> Result<RedisValuePage> {
    // 开区间 "(id" 需要 Redis >= 6.2
    let start = match cursor {
        ValuePageCursor::Start => "-".to_string(),
        ValuePageCursor::AfterId(id) => format!("({id}"),
        other => return Err(cursor_mismatch("Stream", other)),
    };
    let v: RV = redis::cmd("XRANGE")
        .arg(key)
        .arg(start)
        .arg("+")
        .arg("COUNT")
        .arg(max_items)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "XRANGE")?;
    let (entries, raw_count, last_id, skipped) = strict_stream_entries(v)?;
    let next = (raw_count as u32 == max_items)
        .then_some(last_id.map(ValuePageCursor::AfterId))
        .flatten();
    Ok(RedisValuePage {
        items: RedisValue::Stream(entries),
        next,
        skipped,
        ttl_ms: None,
    })
}

#[cfg(test)]
mod tests;
