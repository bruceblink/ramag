//! RedisDriver。实现 KvDriver。每个方法：clone config + pool 句柄 → run_in_tokio → 取 mgr 发命令 → 解码 → 映射错

use async_trait::async_trait;
use ramag_domain::entities::{
    ConnectionConfig, DriverKind, KeyMeta, MAX_REDIS_COLLECTION_BYTES, RedisType, RedisValue,
    RedisValueLoad, RedisValuePage, ScanResult, ValuePageCursor, validate_redis_collection_limit,
    validate_redis_command, validate_redis_key, validate_redis_match_pattern,
    validate_redis_scan_count,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::KvDriver;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Cmd, Value as RV};
use tracing::{debug, warn};

use crate::command::is_write_command;
use crate::errors::map_redis_error;
use crate::pool::PoolCache;
use crate::runtime::run_in_tokio;
use crate::value::{
    decode_hash_pairs, decode_stream_entries, decode_value, decode_zset_with_scores,
};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_NODES: usize = 100_000;
const MAX_RESPONSE_DEPTH: usize = 64;

pub struct RedisDriver {
    pools: PoolCache,
}

impl RedisDriver {
    pub fn new() -> Self {
        Self {
            pools: PoolCache::new(),
        }
    }
}

impl Default for RedisDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// 生产（只读）模式下拦截写操作：命中即记日志并返回 Forbidden。
/// `op` 标识被拦截的操作（DEL / TTL change / 具体命令名），用于日志定位。
fn ensure_writable(config: &ConnectionConfig, op: &str) -> Result<()> {
    if config.production {
        warn!(conn = %config.name, op, "read-only mode: blocked write");
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    Ok(())
}

fn ensure_redis_config(config: &ConnectionConfig) -> Result<()> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if config.driver != DriverKind::Redis {
        return Err(DomainError::InvalidConfig(format!(
            "RedisDriver 不支持 {:?} 类型连接",
            config.driver
        )));
    }
    Ok(())
}

#[async_trait]
impl KvDriver for RedisDriver {
    fn name(&self) -> &'static str {
        "redis"
    }

    fn is_write_command(&self, command: &str) -> bool {
        is_write_command(command)
    }

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()> {
        ensure_redis_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, 0).await?;
            ping(&mut mgr).await
        })
        .await
    }

    async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        ensure_redis_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, 0).await?;
            let info = run_info(&mut mgr, &["server"]).await?;
            Ok(parse_redis_version(&info))
        })
        .await
    }

    async fn db_size(&self, config: &ConnectionConfig, db: u8) -> Result<u64> {
        ensure_redis_config(config)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let n: u64 = redis::cmd("DBSIZE")
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            Ok(n)
        })
        .await
    }

    async fn scan(
        &self,
        config: &ConnectionConfig,
        db: u8,
        cursor: u64,
        match_pattern: Option<&str>,
        type_filter: Option<RedisType>,
        count: u32,
    ) -> Result<ScanResult> {
        ensure_redis_config(config)?;
        validate_redis_scan_count(count)?;
        if let Some(pattern) = match_pattern {
            validate_redis_match_pattern(pattern)?;
        }
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let pattern = match_pattern.map(str::to_owned);
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(cursor);
            if let Some(p) = pattern.as_ref() {
                cmd.arg("MATCH").arg(p);
            }
            // COUNT 仅是 hint
            cmd.arg("COUNT").arg(count.max(1));
            if let Some(t) = type_filter {
                cmd.arg("TYPE").arg(t.as_scan_arg());
            }
            let v: RV = cmd.query_async(&mut mgr).await.map_err(map_redis_error)?;
            parse_scan_response(v)
        })
        .await
    }

    async fn key_type(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<RedisType> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let s: String = redis::cmd("TYPE")
                .arg(&key)
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            Ok(RedisType::parse(&s))
        })
        .await
    }

    async fn key_ttl(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<i64> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let ms: i64 = redis::cmd("PTTL")
                .arg(&key)
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            Ok(ms)
        })
        .await
    }

    async fn get_value(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<RedisValue> {
        Ok(self
            .get_value_limited(config, db, key, DEFAULT_COLLECTION_LIMIT)
            .await?
            .value)
    }

    async fn get_value_limited(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        limit: usize,
    ) -> Result<RedisValueLoad> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        validate_redis_collection_limit(limit)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            // 先 TYPE 再按类型 dispatch
            let t: String = redis::cmd("TYPE")
                .arg(&key)
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            let kind = RedisType::parse(&t);
            debug!(key_bytes = key.len(), ?kind, "get_value dispatch");
            let total = fetch_value_len(&mut mgr, &key, kind).await?;
            let (value, byte_limited) = match kind {
                RedisType::None => Ok((RedisValue::Nil, false)),
                RedisType::String => fetch_string(&mut mgr, &key, total.unwrap_or_default()).await,
                RedisType::List => fetch_list(&mut mgr, &key, limit).await,
                RedisType::Hash => fetch_hash(&mut mgr, &key, limit).await,
                RedisType::Set => fetch_set(&mut mgr, &key, limit).await,
                RedisType::ZSet => fetch_zset(&mut mgr, &key, limit).await,
                RedisType::Stream => fetch_stream(&mut mgr, &key, limit).await,
            }?;
            Ok(RedisValueLoad {
                value,
                total,
                byte_limited,
            })
        })
        .await
    }

    async fn read_value_page(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        kind: Option<RedisType>,
        cursor: ValuePageCursor,
        max_items: u32,
    ) -> Result<RedisValuePage> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        crate::value_page::validate_page_items(max_items)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            crate::value_page::read_page(&mut mgr, &key, kind, cursor, max_items).await
        })
        .await
    }

    async fn write_value_items(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        items: &RedisValue,
    ) -> Result<u64> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        ensure_writable(config, "IMPORT write")?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        let items = items.clone();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            crate::value_page::write_items(&mut mgr, &key, &items).await
        })
        .await
    }

    async fn delete_key(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<bool> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        ensure_writable(config, "DEL")?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let removed: u32 = mgr.del(&key).await.map_err(map_redis_error)?;
            Ok(removed > 0)
        })
        .await
    }

    async fn set_ttl(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        ttl_secs: Option<i64>,
    ) -> Result<bool> {
        ensure_redis_config(config)?;
        validate_redis_key(key)?;
        ensure_writable(config, "TTL change")?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let ok: i64 = match ttl_secs {
                Some(secs) => redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(secs)
                    .query_async(&mut mgr)
                    .await
                    .map_err(map_redis_error)?,
                None => redis::cmd("PERSIST")
                    .arg(&key)
                    .query_async(&mut mgr)
                    .await
                    .map_err(map_redis_error)?,
            };
            Ok(ok == 1)
        })
        .await
    }

    async fn execute_command(
        &self,
        config: &ConnectionConfig,
        db: u8,
        argv: Vec<String>,
    ) -> Result<RedisValue> {
        ensure_redis_config(config)?;
        validate_redis_command(&argv)?;
        if is_write_command(&argv[0]) {
            ensure_writable(config, &argv[0])?;
        }
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let mut cmd = Cmd::new();
            for a in argv {
                cmd.arg(a);
            }
            let v: RV = cmd.query_async(&mut mgr).await.map_err(map_redis_error)?;
            ensure_response_budget(&v, "Redis 命令")?;
            Ok(decode_value(v))
        })
        .await
    }

    async fn info(&self, config: &ConnectionConfig, sections: &[&str]) -> Result<String> {
        ensure_redis_config(config)?;
        validate_info_sections(sections)?;
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let sections: Vec<String> = sections.iter().map(|s| s.to_string()).collect();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, 0).await?;
            let refs: Vec<&str> = sections.iter().map(String::as_str).collect();
            run_info(&mut mgr, &refs).await
        })
        .await
    }

    fn evict_pool(&self, id: &ramag_domain::entities::ConnectionId) {
        // 池按 (ConnectionId, db) 索引，需清空该连接所有 db
        self.pools.evict_all_dbs(id);
        // 该连接的 SSH 隧道一并关闭（编辑配置后下次建连按新参数重建）
        ramag_infra_tunnel::evict(id);
    }
}

fn validate_info_sections(sections: &[&str]) -> Result<()> {
    const MAX_INFO_SECTIONS: usize = 32;
    const MAX_INFO_SECTION_BYTES: usize = 256;

    if sections.len() > MAX_INFO_SECTIONS {
        return Err(DomainError::InvalidConfig(format!(
            "Redis INFO section 超过 {MAX_INFO_SECTIONS} 个上限"
        )));
    }
    for section in sections {
        if section.is_empty()
            || section.len() > MAX_INFO_SECTION_BYTES
            || !section.is_ascii()
            || section
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(DomainError::InvalidConfig(
                "Redis INFO section 必须是至多 256 bytes 的无空白 ASCII 文本".into(),
            ));
        }
    }
    Ok(())
}

async fn ping(mgr: &mut ConnectionManager) -> Result<()> {
    let pong: String = redis::cmd("PING")
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    if pong.eq_ignore_ascii_case("PONG") {
        Ok(())
    } else {
        Err(DomainError::ConnectionFailed(format!(
            "PING 应答异常：{pong}"
        )))
    }
}

async fn run_info(mgr: &mut ConnectionManager, sections: &[&str]) -> Result<String> {
    let mut cmd = redis::cmd("INFO");
    for s in sections {
        cmd.arg(*s);
    }
    let value: RV = cmd.query_async(mgr).await.map_err(map_redis_error)?;
    ensure_response_budget(&value, "INFO")?;
    match value {
        RV::BulkString(bytes) => String::from_utf8(bytes)
            .map_err(|error| DomainError::QueryFailed(format!("INFO 应答非 UTF-8：{error}"))),
        RV::SimpleString(text) => Ok(text),
        other => Err(DomainError::QueryFailed(format!(
            "INFO 应答类型异常：{other:?}"
        ))),
    }
}

/// 从 INFO server 文本提取 redis_version
fn parse_redis_version(info: &str) -> String {
    for line in info.lines() {
        if let Some(rest) = line.strip_prefix("redis_version:") {
            return rest.trim().to_string();
        }
    }
    "unknown".into()
}

/// 集合类型单次加载成员上限：防百万成员 key 一次性拉全量撑爆内存 / 卡死服务端。
/// 超过时只取前 N，UI 侧据 RedisValue 的成员数与实际长度差异提示「仅显示前 N」。
const DEFAULT_COLLECTION_LIMIT: usize = 10_000;
/// 详情渲染同样只保留 4 MiB；从服务端直接按此前缀读取，避免先拉取超大 String。
const MAX_STRING_BYTES: u64 = 4 * 1024 * 1024;

async fn fetch_value_len(
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

async fn fetch_string(
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

fn decode_string_prefix(value: RV, truncated: bool) -> RedisValue {
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

async fn fetch_list(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    // LRANGE 0 N-1 只取前 N，避免 `0 -1` 全量拉取
    let v: RV = redis::cmd("LRANGE")
        .arg(key)
        .arg(0)
        .arg(limit.saturating_sub(1))
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "LRANGE")?;
    let elems = match v {
        RV::Array(a) => a.into_iter().map(decode_value).collect(),
        RV::Nil => return Ok((RedisValue::Nil, false)),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "LRANGE 应答非数组：{other:?}"
            )));
        }
    };
    Ok((RedisValue::List(elems), false))
}

async fn fetch_hash(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    // COUNT 只是 hint，必须续扫游标，不能把第一批误当成完整结果。
    const SCAN_BATCH: usize = 500;
    let mut cursor = 0u64;
    let mut pairs = Vec::with_capacity(limit.min(DEFAULT_COLLECTION_LIMIT));
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    loop {
        let v: RV = redis::cmd("HSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limit.saturating_sub(pairs.len()).clamp(1, SCAN_BATCH))
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

async fn fetch_set(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    const SCAN_BATCH: usize = 500;
    let mut cursor = 0u64;
    let mut elems = Vec::with_capacity(limit.min(DEFAULT_COLLECTION_LIMIT));
    let mut retained_bytes = 0usize;
    let mut byte_limited = false;
    loop {
        let v: RV = redis::cmd("SSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limit.saturating_sub(elems.len()).clamp(1, SCAN_BATCH))
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

async fn fetch_zset(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    // ZRANGE 0 N-1 只取前 N（按 score 升序），避免 `0 -1` 全量拉取
    let v: RV = redis::cmd("ZRANGE")
        .arg(key)
        .arg(0)
        .arg(limit.saturating_sub(1))
        .arg("WITHSCORES")
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "ZRANGE")?;
    decode_zset_with_scores(v).map(|value| (value, false))
}

async fn fetch_stream(
    mgr: &mut ConnectionManager,
    key: &str,
    limit: usize,
) -> Result<(RedisValue, bool)> {
    // XRANGE - + COUNT N 只取前 N 条
    let v: RV = redis::cmd("XRANGE")
        .arg(key)
        .arg("-")
        .arg("+")
        .arg("COUNT")
        .arg(limit)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    ensure_response_budget(&v, "XRANGE")?;
    decode_stream_entries(v).map(|value| (value, false))
}

fn reserve_retained_bytes(current: usize, added: usize, limit: usize) -> Option<usize> {
    current.checked_add(added).filter(|next| *next <= limit)
}

fn redis_value_retained_bytes(value: &RedisValue) -> usize {
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

/// SCAN 系列（HSCAN/SSCAN）应答 `Array([cursor, Array([...])])`，取出成员数组部分
pub(crate) fn scan_parts(v: RV, cmd: &str) -> Result<(u64, RV)> {
    match v {
        RV::Array(mut a) if a.len() == 2 => {
            let payload = a.pop().unwrap_or(RV::Nil);
            let cursor = parse_cursor(a.pop().unwrap_or(RV::Nil), cmd)?;
            Ok((cursor, payload))
        }
        RV::Nil => Ok((0, RV::Nil)),
        other => Err(DomainError::QueryFailed(format!(
            "{cmd} 应答格式异常：{other:?}"
        ))),
    }
}

fn parse_cursor(value: RV, cmd: &str) -> Result<u64> {
    let text = match value {
        RV::BulkString(bytes) => String::from_utf8(bytes)
            .map_err(|e| DomainError::QueryFailed(format!("{cmd} cursor 非 UTF-8：{e}")))?,
        RV::SimpleString(s) => s,
        RV::Int(i) if i >= 0 => return Ok(i as u64),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "{cmd} cursor 类型异常：{other:?}"
            )));
        }
    };
    text.parse::<u64>()
        .map_err(|e| DomainError::QueryFailed(format!("{cmd} cursor 非数字：{e}")))
}

/// SCAN 应答 `Array([cursor_str, Array([key, ...])])`
fn parse_scan_response(v: RV) -> Result<ScanResult> {
    ensure_response_budget(&v, "SCAN")?;
    let (cursor, keys_raw) = scan_parts(v, "SCAN")?;

    let key_arr = match keys_raw {
        RV::Array(a) => a,
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SCAN keys 非数组：{other:?}"
            )));
        }
    };

    let keys = key_arr
        .into_iter()
        .map(|value| {
            let key = match value {
                RV::BulkString(bytes) => String::from_utf8(bytes).map_err(|error| {
                    DomainError::QueryFailed(format!(
                        "SCAN 返回了非 UTF-8 键，当前版本无法安全操作该键：{error}"
                    ))
                })?,
                RV::SimpleString(key) => key,
                other => {
                    return Err(DomainError::QueryFailed(format!(
                        "SCAN 键类型异常：{other:?}"
                    )));
                }
            };
            validate_redis_key(&key).map_err(|error| {
                DomainError::QueryFailed(format!("SCAN 返回了当前版本无法安全操作的键：{error}"))
            })?;
            Ok(KeyMeta::bare(key))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ScanResult { cursor, keys })
}

#[derive(Clone, Copy)]
struct ResponseLimits {
    bytes: usize,
    nodes: usize,
    depth: usize,
}

struct ResponseBudget {
    limits: ResponseLimits,
    bytes: usize,
    nodes: usize,
}

impl ResponseBudget {
    fn visit(&mut self, value: &RV, depth: usize) -> bool {
        if depth > self.limits.depth || self.nodes >= self.limits.nodes {
            return false;
        }
        self.nodes += 1;
        match value {
            RV::BulkString(bytes) => self.add_bytes(bytes.len()),
            RV::SimpleString(text) | RV::VerbatimString { text, .. } => self.add_bytes(text.len()),
            RV::Array(values) | RV::Set(values) | RV::Push { data: values, .. } => values
                .iter()
                .all(|value| self.visit(value, depth.saturating_add(1))),
            RV::Map(pairs) => pairs.iter().all(|(key, value)| {
                self.visit(key, depth.saturating_add(1))
                    && self.visit(value, depth.saturating_add(1))
            }),
            RV::Attribute { data, attributes } => {
                self.visit(data, depth.saturating_add(1))
                    && attributes.iter().all(|(key, value)| {
                        self.visit(key, depth.saturating_add(1))
                            && self.visit(value, depth.saturating_add(1))
                    })
            }
            // 十进制字符串长度小于二进制位数，以 bits 作保守上界避免 to_string 分配。
            RV::BigNumber(number) => {
                self.add_bytes(usize::try_from(number.bits()).unwrap_or(usize::MAX))
            }
            RV::Nil
            | RV::Int(_)
            | RV::Okay
            | RV::Double(_)
            | RV::Boolean(_)
            | RV::ServerError(_) => true,
        }
    }

    fn add_bytes(&mut self, bytes: usize) -> bool {
        self.bytes = self.bytes.saturating_add(bytes);
        self.bytes <= self.limits.bytes
    }
}

pub(crate) fn ensure_response_budget(value: &RV, label: &str) -> Result<()> {
    ensure_response_with_limits(
        value,
        label,
        ResponseLimits {
            bytes: MAX_RESPONSE_BYTES,
            nodes: MAX_RESPONSE_NODES,
            depth: MAX_RESPONSE_DEPTH,
        },
    )
}

fn ensure_response_with_limits(value: &RV, label: &str, limits: ResponseLimits) -> Result<()> {
    let mut budget = ResponseBudget {
        limits,
        bytes: 0,
        nodes: 0,
    };
    if budget.visit(value, 0) {
        Ok(())
    } else {
        Err(DomainError::QueryFailed(format!(
            "{label} 应答超过安全上限（{} MiB、{} 个节点或 {} 层嵌套），请缩小命令范围",
            limits.bytes / 1024 / 1024,
            limits.nodes,
            limits.depth
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_finds_field() {
        let info = "# Server\r\nredis_version:7.2.4\r\nredis_mode:standalone\r\n";
        assert_eq!(parse_redis_version(info), "7.2.4");
    }

    #[test]
    fn parse_version_missing_returns_unknown() {
        assert_eq!(parse_redis_version("# Server\r\nfoo:bar\r\n"), "unknown");
    }

    #[test]
    fn parse_scan_basic() {
        let v = RV::Array(vec![
            RV::BulkString(b"123".to_vec()),
            RV::Array(vec![
                RV::BulkString(b"key1".to_vec()),
                RV::BulkString(b"key2".to_vec()),
            ]),
        ]);
        let r = parse_scan_response(v).unwrap();
        assert_eq!(r.cursor, 123);
        assert_eq!(r.keys.len(), 2);
        assert_eq!(r.keys[0].key, "key1");
        assert_eq!(r.keys[1].key, "key2");
    }

    #[test]
    fn parse_scan_end_cursor_zero() {
        let v = RV::Array(vec![RV::BulkString(b"0".to_vec()), RV::Array(vec![])]);
        let r = parse_scan_response(v).unwrap();
        assert_eq!(r.cursor, 0);
        assert!(r.keys.is_empty());
    }

    #[test]
    fn scan_parts_preserves_cursor_and_payload() {
        let response = RV::Array(vec![
            RV::BulkString(b"9".to_vec()),
            RV::Array(vec![RV::BulkString(b"member".to_vec())]),
        ]);

        let (cursor, payload) = scan_parts(response, "SSCAN").unwrap();

        assert_eq!(cursor, 9);
        assert!(matches!(payload, RV::Array(values) if values.len() == 1));
    }

    #[test]
    fn scan_cursor_rejects_negative_integer() {
        assert!(parse_cursor(RV::Int(-1), "HSCAN").is_err());
        assert!(parse_scan_response(RV::Array(vec![RV::Int(-1), RV::Array(vec![])])).is_err());
    }

    #[test]
    fn scan_rejects_keys_that_cannot_be_addressed_safely() {
        let binary_key = RV::Array(vec![
            RV::BulkString(b"0".to_vec()),
            RV::Array(vec![RV::BulkString(vec![0xff])]),
        ]);
        assert!(parse_scan_response(binary_key).is_err());

        let invalid_type = RV::Array(vec![
            RV::BulkString(b"0".to_vec()),
            RV::Array(vec![RV::Int(42)]),
        ]);
        assert!(parse_scan_response(invalid_type).is_err());
    }

    #[test]
    fn truncated_string_drops_only_incomplete_utf8_tail() {
        let value = decode_string_prefix(RV::BulkString(vec![b'a', 0xe4, 0xb8]), true);
        assert!(matches!(value, RedisValue::Text(text) if text == "a"));

        let binary = decode_string_prefix(RV::BulkString(vec![0xff, 0xfe]), true);
        assert!(matches!(binary, RedisValue::Bytes(bytes) if bytes == vec![0xff, 0xfe]));
    }

    #[test]
    fn response_budget_rejects_bytes_nodes_and_depth() {
        let limits = ResponseLimits {
            bytes: 3,
            nodes: 3,
            depth: 2,
        };
        assert!(
            ensure_response_with_limits(&RV::BulkString(b"abc".to_vec()), "test", limits).is_ok()
        );
        assert!(
            ensure_response_with_limits(&RV::BulkString(b"abcd".to_vec()), "test", limits).is_err()
        );
        assert!(
            ensure_response_with_limits(
                &RV::Array(vec![RV::Int(1), RV::Int(2), RV::Int(3)]),
                "test",
                limits
            )
            .is_err()
        );
        assert!(
            ensure_response_with_limits(
                &RV::Array(vec![RV::Array(vec![RV::Array(vec![RV::Nil])])]),
                "test",
                limits
            )
            .is_err()
        );
    }

    #[test]
    fn info_sections_have_explicit_argument_boundaries() {
        assert!(validate_info_sections(&[]).is_ok());
        assert!(validate_info_sections(&["server", "memory"]).is_ok());
        assert!(validate_info_sections(&[""]).is_err());
        assert!(validate_info_sections(&["bad section"]).is_err());
        let oversized = "x".repeat(257);
        assert!(validate_info_sections(&[oversized.as_str()]).is_err());
        let excessive = ["server"; 33];
        assert!(validate_info_sections(&excessive).is_err());
    }

    #[test]
    fn retained_collection_budget_accepts_boundary_and_rejects_overflow() {
        assert_eq!(reserve_retained_bytes(3, 2, 5), Some(5));
        assert_eq!(reserve_retained_bytes(3, 3, 5), None);
        assert_eq!(reserve_retained_bytes(usize::MAX, 1, usize::MAX), None);

        let short = redis_value_retained_bytes(&RedisValue::Text("a".into()));
        let long = redis_value_retained_bytes(&RedisValue::Text("abcd".into()));
        assert_eq!(long - short, 3);
    }
}
