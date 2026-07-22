//! KvDriver trait：KV 类数据库统一抽象。与 SQL Driver 并列。
//! dyn-safe；连接池按 `(ConnectionId, db)` 缓存（SELECT 是连接级状态）

use async_trait::async_trait;

use crate::entities::{
    ConnectionConfig, ConnectionId, MAX_REDIS_VALUE_PAGE_BATCH, RedisType, RedisValue,
    RedisValueLoad, RedisValuePage, ScanResult, ValuePageCursor,
};
use crate::error::{DomainError, Result};

#[async_trait]
pub trait KvDriver: Send + Sync {
    /// 用于日志 / UI 显示，如 "redis"
    fn name(&self) -> &'static str;

    /// PING
    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()>;

    /// INFO server 的 redis_version
    async fn server_version(&self, config: &ConnectionConfig) -> Result<String>;

    /// DBSIZE
    async fn db_size(&self, config: &ConnectionConfig, db: u8) -> Result<u64>;

    /// SCAN 分批迭代。`cursor`=0 起、返回 0 终；`count` 推荐 100-500（仅 hint）
    async fn scan(
        &self,
        config: &ConnectionConfig,
        db: u8,
        cursor: u64,
        match_pattern: Option<&str>,
        type_filter: Option<RedisType>,
        count: u32,
    ) -> Result<ScanResult>;

    async fn key_type(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<RedisType>;

    /// PTTL：-1=永久，-2=key 不存在，>=0=剩余毫秒
    async fn key_ttl(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<i64>;

    /// 按 TYPE dispatch 取完整 value（GET / LRANGE / HGETALL / SMEMBERS / ZRANGE WITHSCORES / XRANGE）
    /// key 不存在返回 [`RedisValue::Nil`]
    async fn get_value(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<RedisValue>;

    /// 最多加载集合前 `limit` 项，并返回服务端总数；String 由实现按字节上限加载前缀。
    async fn get_value_limited(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        limit: usize,
    ) -> Result<RedisValueLoad>;

    /// 导出用全量分段读：`cursor` 从 [`ValuePageCursor::Start`] 起，按返回的 `next`
    /// 续读到 None。`max_items` 为单页条目上限（String 类型按字节）。
    /// `kind=None` 仅限首页：driver 单次往返内探测 TYPE+PTTL（页里带回 `ttl_ms`，
    /// 类型从 `items` variant 得知）；续读页必须携带类型。
    /// 与 `get_value_limited`（截断预览）不同：逐页覆盖完整内容
    async fn read_value_page(
        &self,
        _config: &ConnectionConfig,
        _db: u8,
        _key: &str,
        _kind: Option<RedisType>,
        _cursor: ValuePageCursor,
        _max_items: u32,
    ) -> Result<RedisValuePage> {
        Err(crate::error::DomainError::NotImplemented(
            "read_value_page".into(),
        ))
    }

    /// 导出首页的有界批量读取。返回顺序必须与 `keys` 一致；默认实现保持串行兼容，
    /// Redis 驱动可在内部复用多路连接并发，调用方仍按原顺序流式写出。
    async fn read_value_first_pages(
        &self,
        config: &ConnectionConfig,
        db: u8,
        keys: &[String],
        max_items: u32,
    ) -> Result<Vec<RedisValuePage>> {
        if keys.len() > MAX_REDIS_VALUE_PAGE_BATCH {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 值页批量读取超过 {MAX_REDIS_VALUE_PAGE_BATCH} 个上限"
            )));
        }
        let mut pages = Vec::with_capacity(keys.len());
        for key in keys {
            pages.push(
                self.read_value_page(config, db, key, None, ValuePageCursor::Start, max_items)
                    .await?,
            );
        }
        Ok(pages)
    }

    /// 导入用分段写：把片段合并进 key（List→RPUSH / Hash→HSET / Set→SADD /
    /// ZSet→ZADD / Text·Bytes→APPEND / Stream→XADD 原 id）。二进制安全；
    /// 生产模式由实现拦截。返回写入条目数
    async fn write_value_items(
        &self,
        _config: &ConnectionConfig,
        _db: u8,
        _key: &str,
        _items: &RedisValue,
    ) -> Result<u64> {
        Err(crate::error::DomainError::NotImplemented(
            "write_value_items".into(),
        ))
    }

    /// 与后端只读保护使用同一分类器，供界面在发请求前禁用 / 拦截写命令。
    fn is_write_command(&self, command: &str) -> bool;

    /// DEL。true=删除了 key，false=本就不存在
    async fn delete_key(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<bool>;

    /// Some(secs)=EXPIRE，None=PERSIST。返回 true 表示 key 存在且成功
    async fn set_ttl(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        ttl_secs: Option<i64>,
    ) -> Result<bool>;

    /// 通用命令执行。argv 拆分后的命令数组，应答按 RESP 类型映射 [`RedisValue`]
    async fn execute_command(
        &self,
        config: &ConnectionConfig,
        db: u8,
        argv: Vec<String>,
    ) -> Result<RedisValue>;

    /// INFO，sections 空切片 = INFO ALL。返回原始文本
    async fn info(&self, config: &ConnectionConfig, sections: &[&str]) -> Result<String>;

    /// 失效指定连接的池缓存。用户改 config 后必须调
    fn evict_pool(&self, _id: &ConnectionId) {}
}
