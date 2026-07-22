//! 导出 / 导入的全量分段读写（`KvDriver::read_value_page` / `write_value_items` 实现体）。
//! 与 driver.rs 的受限预览路径（`get_value_limited`）互不影响：这里逐页覆盖完整内容，
//! 二进制成员经 `RedisValue::Bytes` 保真；仅实体无法表达的条目（二进制 hash field /
//! stream field）跳过并计数

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

/// 单页条目上限；与 32 MiB 传输批次及响应节点预算保持一致。
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
            (RedisType::parse(&type_text), Some(ttl))
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

/// HSCAN 应答严格解码：二进制 field 无法用实体表达，跳过并计数（不做 lossy 转换）
fn strict_hash_pairs(v: RV) -> Result<(Vec<(String, RedisValue)>, u64)> {
    let mut pairs = Vec::new();
    let mut skipped = 0u64;
    match v {
        RV::Nil => {}
        RV::Map(entries) => {
            for (k, value) in entries {
                match field_name(k) {
                    Some(field) => pairs.push((field, decode_value(value))),
                    None => skipped += 1,
                }
            }
        }
        RV::Array(flat) => {
            if flat.len() % 2 != 0 {
                return Err(DomainError::QueryFailed(format!(
                    "HSCAN 应答长度非偶数：{}",
                    flat.len()
                )));
            }
            let mut iter = flat.into_iter();
            while let (Some(k), Some(value)) = (iter.next(), iter.next()) {
                match field_name(k) {
                    Some(field) => pairs.push((field, decode_value(value))),
                    None => skipped += 1,
                }
            }
        }
        other => {
            return Err(DomainError::QueryFailed(format!(
                "HSCAN 应答格式异常：{other:?}"
            )));
        }
    }
    Ok((pairs, skipped))
}

/// XRANGE 应答严格解码。返回（成功条目, 原始条目数, 最后一条原始 id, 跳过数）；
/// 分页游标必须基于原始条目数与原始最后 id，跳过的条目同样推进游标
fn strict_stream_entries(v: RV) -> Result<(Vec<StreamEntry>, usize, Option<String>, u64)> {
    let raw = match v {
        RV::Array(a) => a,
        RV::Nil => Vec::new(),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "XRANGE 应答非数组：{other:?}"
            )));
        }
    };
    let raw_count = raw.len();
    let mut out = Vec::with_capacity(raw_count);
    let mut last_id = None;
    let mut skipped = 0u64;
    for entry in raw {
        let RV::Array(mut parts) = entry else {
            return Err(DomainError::QueryFailed("Stream entry 非数组".into()));
        };
        if parts.len() != 2 {
            return Err(DomainError::QueryFailed(format!(
                "Stream entry 期望 2 元素，实得 {}",
                parts.len()
            )));
        }
        let fields_raw = parts.pop().unwrap_or(RV::Nil);
        let id_raw = parts.pop().unwrap_or(RV::Nil);
        let Some(id) = field_name(id_raw) else {
            return Err(DomainError::QueryFailed("Stream entry id 非 UTF-8".into()));
        };
        last_id = Some(id.clone());
        match strict_stream_fields(fields_raw) {
            Some(fields) => out.push(StreamEntry { id, fields }),
            None => skipped += 1,
        }
    }
    Ok((out, raw_count, last_id, skipped))
}

/// field 与 value 任一非 UTF-8 即整条跳过（实体是 (String, String)）
fn strict_stream_fields(v: RV) -> Option<Vec<(String, String)>> {
    let RV::Array(flat) = v else { return None };
    if flat.len() % 2 != 0 {
        return None;
    }
    let mut pairs = Vec::with_capacity(flat.len() / 2);
    let mut iter = flat.into_iter();
    while let (Some(k), Some(value)) = (iter.next(), iter.next()) {
        pairs.push((field_name(k)?, field_name(value)?));
    }
    Some(pairs)
}

fn field_name(v: RV) -> Option<String> {
    match v {
        RV::SimpleString(s) => Some(s),
        RV::BulkString(bytes) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

async fn append_chunk(mgr: &mut ConnectionManager, key: &str, chunk: &[u8]) -> Result<u64> {
    ensure_member_size(chunk.len())?;
    // 空串单独走 SET：确保空 string 值也能建 key
    if chunk.is_empty() {
        let _: RV = redis::cmd("SET")
            .arg(key)
            .arg(chunk)
            .query_async(mgr)
            .await
            .map_err(map_redis_error)?;
        return Ok(0);
    }
    let _: RV = redis::cmd("APPEND")
        .arg(key)
        .arg(chunk)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    Ok(1)
}

async fn write_members(
    mgr: &mut ConnectionManager,
    key: &str,
    command: &str,
    members: &[RedisValue],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd(command, key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for member in members {
        let arg = member_arg(member)?;
        ensure_member_size(arg.len())?;
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(arg.len()) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd(command, key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(arg.len());
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

async fn write_hash(
    mgr: &mut ConnectionManager,
    key: &str,
    pairs: &[(String, RedisValue)],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd("HSET", key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for (field, value) in pairs {
        let arg = member_arg(value)?;
        ensure_member_size(field.len())?;
        ensure_member_size(arg.len())?;
        let pair_bytes = field.len().saturating_add(arg.len());
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(pair_bytes) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd("HSET", key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(field).arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(pair_bytes);
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

async fn write_zset(
    mgr: &mut ConnectionManager,
    key: &str,
    pairs: &[(RedisValue, f64)],
) -> Result<u64> {
    let mut written = 0u64;
    let mut cmd = new_key_cmd("ZADD", key);
    let mut pending = 0usize;
    let mut pending_bytes = 0usize;
    for (member, score) in pairs {
        let arg = member_arg(member)?;
        ensure_member_size(arg.len())?;
        let score_text = format_score(*score)?;
        let pair_bytes = arg.len().saturating_add(score_text.len());
        if pending > 0
            && (pending >= WRITE_CHUNK_MEMBERS
                || pending_bytes.saturating_add(pair_bytes) > WRITE_CHUNK_BYTES)
        {
            flush(mgr, cmd).await?;
            cmd = new_key_cmd("ZADD", key);
            pending = 0;
            pending_bytes = 0;
        }
        cmd.arg(score_text).arg(arg.as_ref());
        pending += 1;
        pending_bytes = pending_bytes.saturating_add(pair_bytes);
        written += 1;
    }
    if pending > 0 {
        flush(mgr, cmd).await?;
    }
    Ok(written)
}

async fn write_stream(
    mgr: &mut ConnectionManager,
    key: &str,
    entries: &[StreamEntry],
) -> Result<u64> {
    let mut written = 0u64;
    for entry in entries {
        if entry.fields.is_empty() {
            return Err(DomainError::InvalidConfig(format!(
                "Stream entry {} 缺少字段，无法 XADD",
                entry.id
            )));
        }
        let mut cmd = redis::cmd("XADD");
        cmd.arg(key).arg(&entry.id);
        for (field, value) in &entry.fields {
            ensure_member_size(field.len())?;
            ensure_member_size(value.len())?;
            cmd.arg(field).arg(value);
        }
        flush(mgr, cmd).await?;
        written += 1;
    }
    Ok(written)
}

fn new_key_cmd(command: &str, key: &str) -> redis::Cmd {
    let mut cmd = redis::cmd(command);
    cmd.arg(key);
    cmd
}

async fn flush(mgr: &mut ConnectionManager, cmd: redis::Cmd) -> Result<()> {
    let _: RV = cmd.query_async(mgr).await.map_err(map_redis_error)?;
    Ok(())
}

fn ensure_member_size(bytes: usize) -> Result<()> {
    if bytes > MAX_REDIS_COMMAND_ARG_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "Redis 成员超过 {} MiB 上限，无法写入",
            MAX_REDIS_COMMAND_ARG_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

/// 集合成员 → 命令参数字节。Text/Bytes 原样；Int/Float 十进制文本（读取端会把数字
/// 形态的应答解码成 Int/Float，写回时还原成原始文本形态）
fn member_arg(value: &RedisValue) -> Result<Cow<'_, [u8]>> {
    match value {
        RedisValue::Text(text) => Ok(Cow::Borrowed(text.as_bytes())),
        RedisValue::Bytes(bytes) => Ok(Cow::Borrowed(bytes.as_slice())),
        RedisValue::Int(number) => Ok(Cow::Owned(number.to_string().into_bytes())),
        RedisValue::Float(number) => Ok(Cow::Owned(number.to_string().into_bytes())),
        other => Err(DomainError::InvalidConfig(format!(
            "该成员类型不支持导入写入：{}",
            other.display_preview(32)
        ))),
    }
}

fn format_score(score: f64) -> Result<String> {
    if score.is_nan() {
        return Err(DomainError::InvalidConfig("ZSet score 不能是 NaN".into()));
    }
    if score == f64::INFINITY {
        return Ok("+inf".into());
    }
    if score == f64::NEG_INFINITY {
        return Ok("-inf".into());
    }
    Ok(score.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_items_bounds() {
        assert!(validate_page_items(1).is_ok());
        assert!(validate_page_items(MAX_PAGE_ITEMS).is_ok());
        assert!(validate_page_items(0).is_err());
        assert!(validate_page_items(MAX_PAGE_ITEMS + 1).is_err());
    }

    #[test]
    fn cursor_kind_mismatch_rejected() {
        assert!(offset_cursor(&ValuePageCursor::Start, "List").is_ok());
        assert_eq!(
            offset_cursor(&ValuePageCursor::Offset(7), "List").ok(),
            Some(7)
        );
        assert!(offset_cursor(&ValuePageCursor::Scan(1), "List").is_err());
        assert!(scan_cursor(&ValuePageCursor::Offset(1), "Hash").is_err());
        assert_eq!(scan_cursor(&ValuePageCursor::Scan(9), "Hash").ok(), Some(9));
    }

    #[test]
    fn member_arg_covers_scalar_forms() {
        assert_eq!(
            member_arg(&RedisValue::Text("a".into())).unwrap().as_ref(),
            b"a"
        );
        assert_eq!(
            member_arg(&RedisValue::Bytes(vec![0xff])).unwrap().as_ref(),
            &[0xff]
        );
        assert_eq!(member_arg(&RedisValue::Int(-3)).unwrap().as_ref(), b"-3");
        assert_eq!(
            member_arg(&RedisValue::Float(1.5)).unwrap().as_ref(),
            b"1.5"
        );
        assert!(member_arg(&RedisValue::Bool(true)).is_err());
    }

    #[test]
    fn score_formatting_handles_edges() {
        assert_eq!(format_score(2.5).unwrap(), "2.5");
        assert_eq!(format_score(f64::INFINITY).unwrap(), "+inf");
        assert_eq!(format_score(f64::NEG_INFINITY).unwrap(), "-inf");
        assert!(format_score(f64::NAN).is_err());
    }

    #[test]
    fn strict_hash_pairs_skips_binary_fields() {
        let flat = RV::Array(vec![
            RV::BulkString(b"ok".to_vec()),
            RV::BulkString(b"1".to_vec()),
            RV::BulkString(vec![0xff, 0xfe]),
            RV::BulkString(b"2".to_vec()),
        ]);
        let (pairs, skipped) = strict_hash_pairs(flat).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "ok");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn strict_stream_entries_keep_raw_pagination_state() {
        let good = RV::Array(vec![
            RV::BulkString(b"1-1".to_vec()),
            RV::Array(vec![
                RV::BulkString(b"f".to_vec()),
                RV::BulkString(b"v".to_vec()),
            ]),
        ]);
        let bad = RV::Array(vec![
            RV::BulkString(b"1-2".to_vec()),
            RV::Array(vec![
                RV::BulkString(vec![0xff]),
                RV::BulkString(b"v".to_vec()),
            ]),
        ]);
        let (entries, raw_count, last_id, skipped) =
            strict_stream_entries(RV::Array(vec![good, bad])).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(raw_count, 2);
        assert_eq!(last_id.as_deref(), Some("1-2"));
        assert_eq!(skipped, 1);
    }
}
