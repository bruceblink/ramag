//! Redis 按 DB 导出 / 导入（JSONL）。
//!
//! 文件格式（每行一个 JSON 对象）：
//! - 首行 `{"ramag_export":1,"engine":"redis","db":0}`
//! - key 首记录 `{"key":"k","type":"hash","ttl_ms":1234,"value":<片段>}`（ttl_ms null = 永久）
//! - 大 key 续记录 `{"key":"k","more":true,"value":<片段>}`——两端全程流式、内存有界
//!
//! 值片段编码：string `{"text"|"hex"}`；list/set `[item…]`；hash `[[field,item]…]`；
//! 格式：zset `[[item,score]…]`；stream `[{"id","fields":[[f,v]…]}…]`；
//! item = `{"t":文本}|{"x":hex}|{"i":整数}|{"f":浮点}`（二进制走 hex 保真）。
//! TTL 语义：导出时的剩余毫秒，导入完成后 PEXPIRE 生效

mod export;
pub use export::export_redis_db;
pub(crate) use export::{
    ExportKeySource, KeyOutcome, export_key, export_scanned_keys, parse_export_scope,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MAX_REDIS_VALUE_PAGE_BATCH, ProgressFn,
    RedisType, RedisValue, RedisValuePage, StreamEntry, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS,
    TransferSummary, validate_redis_key,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use serde_json::{Value, json};

use super::{
    Reporter, finish_summary, is_cancelled, read_line_bounded, with_export_sink, write_json_line,
};
use crate::usecases::RedisService;

/// 较大的 `SCAN COUNT` 可减少网络往返。
const SCAN_BATCH: u32 = 5_000;
/// 单个容器 key 的分页条目数。
pub(crate) const PAGE_ITEMS: u32 = TRANSFER_BATCH_ITEMS as u32;
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;
const PROGRESS_EVERY_KEYS: u32 = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedisExportScope {
    Key(String),
    Prefix(String),
}

pub async fn import_redis_db(
    svc: &RedisService,
    config: &ConnectionConfig,
    target_db: Option<u8>,
    path: &Path,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_redis(config)?;
    if config.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if policy == ConflictPolicy::Merge {
        return Err(DomainError::InvalidConfig(
            "Redis 导入不支持合并策略（list / string 无法条目级去重）".into(),
        ));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);

    let mut header_line = String::new();
    read_line_bounded(
        &mut reader,
        &mut header_line,
        MAX_LINE_BYTES,
        "Redis 导入文件",
    )?;
    let header: Value = serde_json::from_str(header_line.trim())
        .map_err(|_| DomainError::InvalidConfig("文件首行不是有效的导出头".into()))?;
    if header.get("ramag_export").and_then(Value::as_u64) != Some(1)
        || header.get("engine").and_then(Value::as_str) != Some("redis")
    {
        return Err(DomainError::InvalidConfig(
            "文件不是 Redis 导出（engine 不匹配）".into(),
        ));
    }
    let export_scope = parse_export_scope(&header)?;
    let file_db = header
        .get("db")
        .and_then(Value::as_u64)
        .ok_or_else(|| DomainError::InvalidConfig("Redis 导出文件头缺少有效的 db 字段".into()))?;
    let file_db = u8::try_from(file_db)
        .map_err(|_| DomainError::InvalidConfig("Redis 导出文件头的 db 超出 0–255 范围".into()))?;
    let db = target_db.unwrap_or(file_db);

    let mut summary = TransferSummary::default();
    let mut reporter = Reporter::new(progress);
    reporter.stage("导入 key", format!("DB {db}"));

    // 跨行跟踪大 key，完成后恢复 TTL。
    let mut current: Option<KeyCtx> = None;
    let mut line = String::new();
    loop {
        let read = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES, "Redis 导入文件")?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(trimmed)
            .map_err(|error| DomainError::InvalidConfig(format!("JSONL 解析失败：{error}")))?;
        let Some(key) = record.get("key").and_then(Value::as_str) else {
            summary.push_warning(format!(
                "无法识别的记录被跳过：{}",
                &trimmed[..trimmed.len().min(80)]
            ));
            continue;
        };
        if export_scope
            .as_ref()
            .is_some_and(|scope| !scope.contains(key))
        {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 对象文件包含范围外 Key「{key}」"
            )));
        }
        let is_more = record.get("more").and_then(Value::as_bool) == Some(true);

        if !is_more {
            if let Some(ctx) = current.take() {
                finalize_key(svc, config, db, ctx, &mut summary).await?;
            }
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            let mut ctx = KeyCtx {
                key: key.to_string(),
                kind: record
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                ttl_ms: record.get("ttl_ms").and_then(Value::as_i64),
                skip: false,
            };
            let exists = !matches!(svc.key_type(config, db, key).await?, RedisType::None);
            if exists {
                match policy {
                    // `Merge` 已在入口拒绝，此处按跳过防御处理。
                    ConflictPolicy::Skip | ConflictPolicy::Merge => {
                        ctx.skip = true;
                        summary.skipped += 1;
                    }
                    ConflictPolicy::Fail => {
                        return Err(DomainError::QueryFailed(format!(
                            "key「{key}」已存在（冲突策略：报错停止）"
                        )));
                    }
                    ConflictPolicy::Overwrite => {
                        svc.delete_key(config, db, key).await?;
                    }
                }
            }
            if !ctx.skip {
                write_record_value(
                    svc,
                    config,
                    db,
                    key,
                    ctx.kind.as_deref(),
                    &record,
                    &mut summary,
                )
                .await?;
                summary.objects += 1;
            }
            current = Some(ctx);
            reporter.snapshot.objects_done += 1;
            reporter.snapshot.items_done = summary.items;
            reporter.emit_every(PROGRESS_EVERY_KEYS);
            continue;
        }

        // 续记录沿用首记录类型，避免混淆形态相同的 Set 和 List。
        match current.as_ref() {
            Some(ctx) if ctx.key == key => {
                if !ctx.skip {
                    write_record_value(
                        svc,
                        config,
                        db,
                        key,
                        ctx.kind.as_deref(),
                        &record,
                        &mut summary,
                    )
                    .await?;
                }
            }
            _ => {
                return Err(DomainError::InvalidConfig(format!(
                    "key {key} 的续记录缺少首记录，文件损坏"
                )));
            }
        }
        if is_cancelled(cancel) {
            summary.cancelled = true;
            return Ok(finish_summary(summary, start));
        }
    }
    if let Some(ctx) = current.take() {
        finalize_key(svc, config, db, ctx, &mut summary).await?;
    }
    reporter.snapshot.items_done = summary.items;
    reporter.emit();
    Ok(finish_summary(summary, start))
}

struct KeyCtx {
    key: String,
    kind: Option<String>,
    ttl_ms: Option<i64>,
    skip: bool,
}

async fn write_record_value(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    key: &str,
    kind: Option<&str>,
    record: &Value,
    summary: &mut TransferSummary,
) -> Result<()> {
    let fragment = record
        .get("value")
        .ok_or_else(|| DomainError::InvalidConfig(format!("key {key} 记录缺少 value")))?;
    let items = decode_fragment(kind, fragment)
        .map_err(|error| DomainError::InvalidConfig(format!("key {key}：{}", error.message())))?;
    summary.items += svc.write_value_items(config, db, key, &items).await?;
    Ok(())
}

/// 按导出时的剩余 TTL 恢复过期时间。
async fn finalize_key(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    ctx: KeyCtx,
    summary: &mut TransferSummary,
) -> Result<()> {
    if ctx.skip {
        return Ok(());
    }
    if let Some(ttl_ms) = ctx.ttl_ms.filter(|ttl| *ttl > 0) {
        let reply = svc
            .execute_command(
                config,
                db,
                vec!["PEXPIRE".into(), ctx.key.clone(), ttl_ms.to_string()],
            )
            .await?;
        if !matches!(reply, RedisValue::Int(1)) {
            summary.push_warning(format!("key {}：TTL 恢复未生效", ctx.key));
        }
    }
    Ok(())
}

pub(crate) fn ensure_redis(config: &ConnectionConfig) -> Result<()> {
    if config.driver != DriverKind::Redis {
        return Err(DomainError::InvalidConfig("该操作仅支持 Redis 连接".into()));
    }
    Ok(())
}

fn value_kind(items: &RedisValue) -> Option<RedisType> {
    match items {
        RedisValue::Nil => None,
        RedisValue::Text(_) | RedisValue::Bytes(_) => Some(RedisType::String),
        RedisValue::List(_) => Some(RedisType::List),
        RedisValue::Hash(_) => Some(RedisType::Hash),
        RedisValue::Set(_) => Some(RedisType::Set),
        RedisValue::ZSet(_) => Some(RedisType::ZSet),
        RedisValue::Stream(_) => Some(RedisType::Stream),
        _ => None,
    }
}

fn kind_name(kind: RedisType) -> &'static str {
    match kind {
        RedisType::String => "string",
        RedisType::List => "list",
        RedisType::Hash => "hash",
        RedisType::Set => "set",
        RedisType::ZSet => "zset",
        RedisType::Stream => "stream",
        RedisType::None => "none",
    }
}

/// 将值片段编码为 JSON，并返回条目数。
fn encode_fragment(items: &RedisValue) -> Result<(Value, u64)> {
    Ok(match items {
        RedisValue::Text(text) => (json!({"text": text}), 1),
        RedisValue::Bytes(bytes) => (json!({"hex": hex::encode(bytes)}), 1),
        RedisValue::List(members) | RedisValue::Set(members) => {
            let encoded: Vec<Value> = members.iter().map(encode_item).collect::<Result<_>>()?;
            let count = encoded.len() as u64;
            (Value::Array(encoded), count)
        }
        RedisValue::Hash(pairs) => {
            let encoded: Vec<Value> = pairs
                .iter()
                .map(|(field, value)| Ok(json!([field, encode_item(value)?])))
                .collect::<Result<_>>()?;
            let count = encoded.len() as u64;
            (Value::Array(encoded), count)
        }
        RedisValue::ZSet(pairs) => {
            let encoded: Vec<Value> = pairs
                .iter()
                .map(|(member, score)| Ok(json!([encode_item(member)?, score])))
                .collect::<Result<_>>()?;
            let count = encoded.len() as u64;
            (Value::Array(encoded), count)
        }
        RedisValue::Stream(entries) => {
            let encoded: Vec<Value> = entries
                .iter()
                .map(|entry| json!({"id": entry.id, "fields": entry.fields}))
                .collect();
            let count = encoded.len() as u64;
            (Value::Array(encoded), count)
        }
        RedisValue::Nil => (Value::Null, 0),
        other => {
            return Err(DomainError::InvalidConfig(format!(
                "值类型无法导出：{}",
                other.display_preview(32)
            )));
        }
    })
}

fn encode_item(value: &RedisValue) -> Result<Value> {
    Ok(match value {
        RedisValue::Text(text) => json!({"t": text}),
        RedisValue::Bytes(bytes) => json!({"x": hex::encode(bytes)}),
        RedisValue::Int(number) => json!({"i": number}),
        RedisValue::Float(number) => json!({"f": number}),
        other => {
            return Err(DomainError::InvalidConfig(format!(
                "成员类型无法导出：{}",
                other.display_preview(32)
            )));
        }
    })
}

/// 将 JSON 解码为值片段；字符串片段可自行描述类型。
fn decode_fragment(kind: Option<&str>, fragment: &Value) -> Result<RedisValue> {
    if let Some(object) = fragment.as_object() {
        if let Some(text) = object.get("text").and_then(Value::as_str) {
            return Ok(RedisValue::Text(text.to_string()));
        }
        if let Some(hex_text) = object.get("hex").and_then(Value::as_str) {
            return Ok(RedisValue::Bytes(decode_hex(hex_text)?));
        }
        return Err(DomainError::InvalidConfig("string 片段格式异常".into()));
    }
    let array = fragment
        .as_array()
        .ok_or_else(|| DomainError::InvalidConfig("值片段必须是对象或数组".into()))?;
    let kind = kind.ok_or_else(|| DomainError::InvalidConfig("集合片段缺少类型信息".into()))?;
    match kind {
        "list" => Ok(RedisValue::List(
            array.iter().map(decode_item).collect::<Result<_>>()?,
        )),
        "set" => Ok(RedisValue::Set(
            array.iter().map(decode_item).collect::<Result<_>>()?,
        )),
        "hash" => Ok(RedisValue::Hash(
            array
                .iter()
                .map(|pair| {
                    let pair = pair
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or_else(|| DomainError::InvalidConfig("hash 片段格式异常".into()))?;
                    let field = pair[0].as_str().ok_or_else(|| {
                        DomainError::InvalidConfig("hash field 必须是字符串".into())
                    })?;
                    Ok((field.to_string(), decode_item(&pair[1])?))
                })
                .collect::<Result<_>>()?,
        )),
        "zset" => Ok(RedisValue::ZSet(
            array
                .iter()
                .map(|pair| {
                    let pair = pair
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or_else(|| DomainError::InvalidConfig("zset 片段格式异常".into()))?;
                    let score = pair[1].as_f64().ok_or_else(|| {
                        DomainError::InvalidConfig("zset score 必须是数字".into())
                    })?;
                    Ok((decode_item(&pair[0])?, score))
                })
                .collect::<Result<_>>()?,
        )),
        "stream" => Ok(RedisValue::Stream(
            array
                .iter()
                .map(|entry| {
                    let id = entry
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| DomainError::InvalidConfig("stream entry 缺少 id".into()))?;
                    let fields = entry
                        .get("fields")
                        .and_then(Value::as_array)
                        .ok_or_else(|| {
                            DomainError::InvalidConfig("stream entry 缺少 fields".into())
                        })?
                        .iter()
                        .map(|pair| {
                            let pair =
                                pair.as_array().filter(|p| p.len() == 2).ok_or_else(|| {
                                    DomainError::InvalidConfig("stream field 格式异常".into())
                                })?;
                            match (pair[0].as_str(), pair[1].as_str()) {
                                (Some(field), Some(value)) => {
                                    Ok((field.to_string(), value.to_string()))
                                }
                                _ => Err(DomainError::InvalidConfig(
                                    "stream field 必须是字符串".into(),
                                )),
                            }
                        })
                        .collect::<Result<Vec<(String, String)>>>()?;
                    Ok(StreamEntry {
                        id: id.to_string(),
                        fields,
                    })
                })
                .collect::<Result<_>>()?,
        )),
        other => Err(DomainError::InvalidConfig(format!("未知值类型：{other}"))),
    }
}

fn decode_item(value: &Value) -> Result<RedisValue> {
    let object = value
        .as_object()
        .ok_or_else(|| DomainError::InvalidConfig("成员必须是对象".into()))?;
    if let Some(text) = object.get("t").and_then(Value::as_str) {
        return Ok(RedisValue::Text(text.to_string()));
    }
    if let Some(hex_text) = object.get("x").and_then(Value::as_str) {
        return Ok(RedisValue::Bytes(decode_hex(hex_text)?));
    }
    if let Some(number) = object.get("i").and_then(Value::as_i64) {
        return Ok(RedisValue::Int(number));
    }
    if let Some(number) = object.get("f").and_then(Value::as_f64) {
        return Ok(RedisValue::Float(number));
    }
    Err(DomainError::InvalidConfig("成员编码无法识别".into()))
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    hex::decode(text).map_err(|error| DomainError::InvalidConfig(format!("hex 解码失败：{error}")))
}

#[cfg(test)]
mod tests;
