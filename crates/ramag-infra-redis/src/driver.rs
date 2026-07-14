//! RedisDriver。实现 KvDriver。每个方法：clone config + pool 句柄 → run_in_tokio → 取 mgr 发命令 → 解码 → 映射错

use async_trait::async_trait;
use ramag_domain::entities::{
    ConnectionConfig, KeyMeta, RedisType, RedisValue, RedisValueLoad, ScanResult,
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

#[async_trait]
impl KvDriver for RedisDriver {
    fn name(&self) -> &'static str {
        "redis"
    }

    fn is_write_command(&self, command: &str) -> bool {
        is_write_command(command)
    }

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()> {
        let config = config.clone();
        let pools = self.pools.clone_handle();
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, 0).await?;
            ping(&mut mgr).await
        })
        .await
    }

    async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
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
        let config = config.clone();
        let pools = self.pools.clone_handle();
        let key = key.to_owned();
        let limit = limit.max(1);
        run_in_tokio(async move {
            let mut mgr = pools.get_or_create(&config, db).await?;
            // 先 TYPE 再按类型 dispatch
            let t: String = redis::cmd("TYPE")
                .arg(&key)
                .query_async(&mut mgr)
                .await
                .map_err(map_redis_error)?;
            let kind = RedisType::parse(&t);
            debug!(?key, ?kind, "get_value dispatch");
            let total = fetch_collection_len(&mut mgr, &key, kind).await?;
            let value = match kind {
                RedisType::None => Ok(RedisValue::Nil),
                RedisType::String => fetch_string(&mut mgr, &key).await,
                RedisType::List => fetch_list(&mut mgr, &key, limit).await,
                RedisType::Hash => fetch_hash(&mut mgr, &key, limit).await,
                RedisType::Set => fetch_set(&mut mgr, &key, limit).await,
                RedisType::ZSet => fetch_zset(&mut mgr, &key, limit).await,
                RedisType::Stream => fetch_stream(&mut mgr, &key, limit).await,
            }?;
            Ok(RedisValueLoad { value, total })
        })
        .await
    }

    async fn delete_key(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<bool> {
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
        if argv.is_empty() {
            return Err(DomainError::InvalidConfig(
                "命令为空，至少需要命令名".into(),
            ));
        }
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
            Ok(decode_value(v))
        })
        .await
    }

    async fn info(&self, config: &ConnectionConfig, sections: &[&str]) -> Result<String> {
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
    let s: String = cmd.query_async(mgr).await.map_err(map_redis_error)?;
    Ok(s)
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

async fn fetch_collection_len(
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
        RedisType::String | RedisType::None => return Ok(None),
    };
    let total: u64 = redis::cmd(command)
        .arg(key)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    Ok(Some(total))
}

async fn fetch_string(mgr: &mut ConnectionManager, key: &str) -> Result<RedisValue> {
    let v: RV = redis::cmd("GET")
        .arg(key)
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    Ok(decode_value(v))
}

async fn fetch_list(mgr: &mut ConnectionManager, key: &str, limit: usize) -> Result<RedisValue> {
    // LRANGE 0 N-1 只取前 N，避免 `0 -1` 全量拉取
    let v: RV = redis::cmd("LRANGE")
        .arg(key)
        .arg(0)
        .arg(limit.saturating_sub(1))
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    let elems = match v {
        RV::Array(a) => a.into_iter().map(decode_value).collect(),
        RV::Nil => return Ok(RedisValue::Nil),
        other => {
            return Err(DomainError::QueryFailed(format!(
                "LRANGE 应答非数组：{other:?}"
            )));
        }
    };
    Ok(RedisValue::List(elems))
}

async fn fetch_hash(mgr: &mut ConnectionManager, key: &str, limit: usize) -> Result<RedisValue> {
    // COUNT 只是 hint，必须续扫游标，不能把第一批误当成完整结果。
    let mut cursor = 0u64;
    let mut pairs = Vec::with_capacity(limit.min(DEFAULT_COLLECTION_LIMIT));
    loop {
        let v: RV = redis::cmd("HSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limit.saturating_sub(pairs.len()).max(1))
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        let (next, payload) = scan_parts(v, "HSCAN")?;
        let RedisValue::Hash(batch) = decode_hash_pairs(payload)? else {
            return Err(DomainError::QueryFailed("HSCAN 解码结果类型异常".into()));
        };
        pairs.extend(batch.into_iter().take(limit.saturating_sub(pairs.len())));
        cursor = next;
        if cursor == 0 || pairs.len() >= limit {
            break;
        }
    }
    Ok(RedisValue::Hash(pairs))
}

async fn fetch_set(mgr: &mut ConnectionManager, key: &str, limit: usize) -> Result<RedisValue> {
    let mut cursor = 0u64;
    let mut elems = Vec::with_capacity(limit.min(DEFAULT_COLLECTION_LIMIT));
    loop {
        let v: RV = redis::cmd("SSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(limit.saturating_sub(elems.len()).max(1))
            .query_async(&mut *mgr)
            .await
            .map_err(map_redis_error)?;
        let (next, payload) = scan_parts(v, "SSCAN")?;
        match payload {
            RV::Array(a) => elems.extend(
                a.into_iter()
                    .map(decode_value)
                    .take(limit.saturating_sub(elems.len())),
            ),
            RV::Nil => {}
            other => {
                return Err(DomainError::QueryFailed(format!(
                    "SSCAN 应答非数组：{other:?}"
                )));
            }
        }
        cursor = next;
        if cursor == 0 || elems.len() >= limit {
            break;
        }
    }
    Ok(RedisValue::Set(elems))
}

async fn fetch_zset(mgr: &mut ConnectionManager, key: &str, limit: usize) -> Result<RedisValue> {
    // ZRANGE 0 N-1 只取前 N（按 score 升序），避免 `0 -1` 全量拉取
    let v: RV = redis::cmd("ZRANGE")
        .arg(key)
        .arg(0)
        .arg(limit.saturating_sub(1))
        .arg("WITHSCORES")
        .query_async(mgr)
        .await
        .map_err(map_redis_error)?;
    decode_zset_with_scores(v)
}

async fn fetch_stream(mgr: &mut ConnectionManager, key: &str, limit: usize) -> Result<RedisValue> {
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
    decode_stream_entries(v)
}

/// SCAN 系列（HSCAN/SSCAN）应答 `Array([cursor, Array([...])])`，取出成员数组部分
fn scan_parts(v: RV, cmd: &str) -> Result<(u64, RV)> {
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
    let mut top = match v {
        RV::Array(a) => a,
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SCAN 应答非数组：{other:?}"
            )));
        }
    };
    if top.len() != 2 {
        return Err(DomainError::QueryFailed(format!(
            "SCAN 应答应有 2 元素，实得 {}",
            top.len()
        )));
    }
    let keys_raw = top.remove(1);
    let cursor_raw = top.remove(0);

    let cursor = match cursor_raw {
        RV::BulkString(bytes) => std::str::from_utf8(&bytes)
            .map_err(|e| DomainError::QueryFailed(format!("SCAN cursor 非 utf-8：{e}")))?
            .parse::<u64>()
            .map_err(|e| DomainError::QueryFailed(format!("SCAN cursor 非数字：{e}")))?,
        RV::SimpleString(s) => s
            .parse::<u64>()
            .map_err(|e| DomainError::QueryFailed(format!("SCAN cursor 非数字：{e}")))?,
        RV::Int(i) => i as u64,
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SCAN cursor 类型异常：{other:?}"
            )));
        }
    };

    let key_arr = match keys_raw {
        RV::Array(a) => a,
        other => {
            return Err(DomainError::QueryFailed(format!(
                "SCAN keys 非数组：{other:?}"
            )));
        }
    };

    let keys: Vec<KeyMeta> = key_arr
        .into_iter()
        .filter_map(|v| match decode_value(v) {
            RedisValue::Text(s) => Some(KeyMeta::bare(s)),
            RedisValue::Bytes(b) => Some(KeyMeta::bare(String::from_utf8_lossy(&b).into_owned())),
            _ => None,
        })
        .collect();

    Ok(ScanResult { cursor, keys })
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
    }
}
