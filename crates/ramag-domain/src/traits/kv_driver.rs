//! KV 数据库接口；连接池按 `(ConnectionId, db)` 隔离。

use async_trait::async_trait;

use crate::entities::{
    ConnectionConfig, ConnectionId, MAX_REDIS_KEY_TYPE_BATCH, MAX_REDIS_VALUE_PAGE_BATCH,
    RedisType, RedisValue, RedisValueLoad, RedisValuePage, ScanResult, ValuePageCursor,
};
use crate::error::{DomainError, Result};

#[async_trait]
pub trait KvDriver: Send + Sync {
    /// 用于日志 / UI 显示，如 "redis"
    fn name(&self) -> &'static str;

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()>;

    async fn server_version(&self, config: &ConnectionConfig) -> Result<String>;

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

    /// 批量读取类型，返回顺序必须与 `keys` 一致；Redis 实现应使用 Pipeline 降低网络往返。
    async fn key_types(
        &self,
        config: &ConnectionConfig,
        db: u8,
        keys: &[String],
    ) -> Result<Vec<RedisType>> {
        if keys.len() > MAX_REDIS_KEY_TYPE_BATCH {
            return Err(DomainError::InvalidConfig(format!(
                "Redis 类型批量读取超过 {MAX_REDIS_KEY_TYPE_BATCH} 个上限"
            )));
        }
        let mut types = Vec::with_capacity(keys.len());
        for key in keys {
            types.push(self.key_type(config, db, key).await?);
        }
        Ok(types)
    }

    /// PTTL：-1=永久，-2=key 不存在，>=0=剩余毫秒
    async fn key_ttl(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<i64>;

    /// 根据类型读取完整值；key 不存在时返回 [`RedisValue::Nil`]。
    async fn get_value(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<RedisValue>;

    /// 最多加载集合前 `limit` 项，并返回服务端总数；String 由实现按字节上限加载前缀。
    async fn get_value_limited(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        limit: usize,
    ) -> Result<RedisValueLoad>;

    /// 从 `Start` 按 `next` 分页读取完整值；仅首页允许自动探测类型。
    /// `max_items` 对字符串表示字节数，对其他类型表示条目数。
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

    /// 返回顺序必须与 `keys` 一致；默认串行读取。
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

    /// 将片段追加到 key，返回写入条目数；实现必须保持二进制安全并执行只读保护。
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

    /// 返回是否实际删除了 key。
    async fn delete_key(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<bool>;

    /// `Some` 设置过期秒数，`None` 移除过期时间；返回 key 是否存在。
    async fn set_ttl(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        ttl_secs: Option<i64>,
    ) -> Result<bool>;

    /// 执行已拆分的命令参数，并将 RESP 应答映射为 [`RedisValue`]。
    async fn execute_command(
        &self,
        config: &ConnectionConfig,
        db: u8,
        argv: Vec<String>,
    ) -> Result<RedisValue>;

    /// sections 为空时读取全部 INFO，返回原始文本。
    async fn info(&self, config: &ConnectionConfig, sections: &[&str]) -> Result<String>;

    /// 配置变更后使对应连接池失效。
    fn evict_pool(&self, _id: &ConnectionId) {}
}
