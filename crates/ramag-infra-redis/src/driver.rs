//! Redis 驱动：校验参数后在专用 Tokio runtime 执行命令。

use async_trait::async_trait;
use futures::stream::{self, StreamExt as _};
use ramag_domain::entities::{
    ConnectionConfig, DriverKind, INTERACTIVE_RESULT_WARNING_BYTES, MAX_REDIS_COLLECTION_BYTES,
    MAX_REDIS_COLLECTION_ITEMS, MAX_REDIS_KEY_TYPE_BATCH, MAX_REDIS_VALUE_PAGE_BATCH, RedisType,
    RedisValue, RedisValueLoad, RedisValuePage, ScanResult, StreamEntry, ValuePageCursor,
    validate_redis_collection_limit, validate_redis_command, validate_redis_key,
    validate_redis_match_pattern, validate_redis_scan_count,
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

mod protocol;
mod value_load;

use protocol::parse_scan_response;
#[cfg(test)]
use protocol::{MAX_RESPONSE_BYTES, ResponseLimits, ensure_response_with_limits, parse_cursor};
pub(crate) use protocol::{ensure_response_budget, scan_parts};
use value_load::*;

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

/// 生产模式下拦截写操作并记录操作名。
fn ensure_writable(config: &ConnectionConfig, op: &str) -> Result<()> {
    if config.production {
        warn!(operation = op, connection = %config.name, "read-only write blocked");
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
            parse_redis_version(&info)
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
            // COUNT 仅为提示值。
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

    async fn key_types(
        &self,
        config: &ConnectionConfig,
        db: u8,
        keys: &[String],
    ) -> Result<Vec<RedisType>> {
        const PIPELINE_CHUNK: usize = 512;

        ensure_redis_config(config)?;
        if keys.len() > MAX_REDIS_KEY_TYPE_BATCH {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 类型批量读取超过 {MAX_REDIS_KEY_TYPE_BATCH} 个上限"
            )));
        }
        for key in keys {
            validate_redis_key(key)?;
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let config = config.clone();
        let keys = keys.to_vec();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            let mut types = Vec::with_capacity(keys.len());
            for chunk in keys.chunks(PIPELINE_CHUNK) {
                let mut pipeline = redis::pipe();
                for key in chunk {
                    pipeline.cmd("TYPE").arg(key);
                }
                let values: Vec<String> = pipeline
                    .query_async(&mut mgr)
                    .await
                    .map_err(map_redis_error)?;
                if values.len() != chunk.len() {
                    return Err(DomainError::Other(format!(
                        "Redis TYPE Pipeline 响应数量异常：期望 {}，实际 {}",
                        chunk.len(),
                        values.len()
                    )));
                }
                types.extend(values.iter().map(|value| RedisType::parse(value)));
            }
            Ok(types)
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
            let t: String = redis::cmd("TYPE")
                .arg(&key)
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            let kind = RedisType::parse(&t);
            debug!(
                operation = "redis_value_load",
                key_bytes = key.len(),
                ?kind,
                "value load dispatched"
            );
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
            let memory_warning =
                redis_value_retained_bytes(&value) >= INTERACTIVE_RESULT_WARNING_BYTES;
            Ok(RedisValueLoad {
                value,
                total,
                byte_limited,
                memory_warning,
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

    async fn read_value_first_pages(
        &self,
        config: &ConnectionConfig,
        db: u8,
        keys: &[String],
        max_items: u32,
    ) -> Result<Vec<RedisValuePage>> {
        ensure_redis_config(config)?;
        crate::value_page::validate_page_items(max_items)?;
        if keys.len() > MAX_REDIS_VALUE_PAGE_BATCH {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 值页批量读取超过 {MAX_REDIS_VALUE_PAGE_BATCH} 个上限"
            )));
        }
        for key in keys {
            validate_redis_key(key)?;
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let config = config.clone();
        let keys = keys.to_vec();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mgr = pools.get_or_create(&config, db).await?;
            let reads = keys.into_iter().map(|key| {
                let mut connection = mgr.clone();
                async move {
                    crate::value_page::read_page(
                        &mut connection,
                        &key,
                        None,
                        ValuePageCursor::Start,
                        max_items,
                    )
                    .await
                }
            });
            let results = stream::iter(reads)
                .buffered(MAX_REDIS_VALUE_PAGE_BATCH)
                .collect::<Vec<_>>()
                .await;
            results.into_iter().collect()
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
        validate_ttl_secs(ttl_secs)?;
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
        // 清理该连接所有 DB 的连接池和 SSH 隧道。
        self.pools.evict_all_dbs(id);
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

/// 从 INFO 提取 Redis 版本。
fn parse_redis_version(info: &str) -> Result<String> {
    for line in info.lines() {
        if let Some(rest) = line.strip_prefix("redis_version:") {
            let version = rest.trim();
            if !version.is_empty() {
                return Ok(version.to_string());
            }
        }
    }
    Err(DomainError::QueryFailed(
        "Redis INFO server 应答缺少 redis_version".into(),
    ))
}

fn validate_ttl_secs(ttl_secs: Option<i64>) -> Result<()> {
    if ttl_secs.is_some_and(|seconds| seconds <= 0) {
        return Err(DomainError::InvalidConfig(
            "Redis TTL 必须是正秒数；零或负数会立即删除键".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
