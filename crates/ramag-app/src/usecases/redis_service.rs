//! Redis 连接与键值操作服务。

use std::sync::Arc;

use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DriverKind, KeyMeta, MAX_REDIS_SCAN_ALL_KEYS, RedisType,
    RedisValue, RedisValueLoad, RedisValuePage, ScanResult, ValuePageCursor,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{KvDriver, Storage};

pub struct RedisService {
    driver: Arc<dyn KvDriver>,
    storage: Arc<dyn Storage>,
}

impl RedisService {
    pub fn new(driver: Arc<dyn KvDriver>, storage: Arc<dyn Storage>) -> Self {
        Self { driver, storage }
    }

    /// 仅列出 Redis 连接。
    pub async fn list(&self) -> Result<Vec<ConnectionConfig>> {
        let all = self.storage.list_connections().await?;
        Ok(all
            .into_iter()
            .filter(|c| matches!(c.driver, DriverKind::Redis))
            .collect())
    }

    /// 按 ID 获取连接，不限制驱动类型。
    pub async fn get(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        self.storage.get_connection(id).await
    }

    pub async fn save(&self, config: &ConnectionConfig) -> Result<()> {
        let result = self.storage.save_connection(config).await;
        match &result {
            Ok(()) => tracing::info!(connection_id = %config.id, "redis connection saved"),
            Err(error) => {
                tracing::error!(error = %error, connection_id = %config.id, "save redis connection failed")
            }
        }
        result
    }

    pub async fn delete(&self, id: &ConnectionId) -> Result<()> {
        let result = self.storage.delete_connection(id).await;
        match &result {
            Ok(()) => tracing::info!(connection_id = %id, "redis connection deleted"),
            Err(error) => {
                tracing::error!(error = %error, connection_id = %id, "delete redis connection failed")
            }
        }
        result
    }

    pub async fn test(&self, config: &ConnectionConfig) -> Result<()> {
        let started = std::time::Instant::now();
        let result = self.driver.test_connection(config).await;
        match &result {
            Ok(()) => {
                tracing::info!(connection_id = %config.id, elapsed_ms = started.elapsed().as_millis(), "redis connection test succeeded")
            }
            Err(error) => {
                tracing::warn!(error = %error, connection_id = %config.id, elapsed_ms = started.elapsed().as_millis(), "redis connection test failed")
            }
        }
        result
    }

    pub async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        self.driver.server_version(config).await
    }

    /// 清除该连接所有数据库的连接池缓存。
    pub fn evict_pool(&self, id: &ConnectionId) {
        self.driver.evict_pool(id);
    }

    /// 一次性扫描完整数据库；大库慎用。
    pub async fn scan_all(
        &self,
        config: &ConnectionConfig,
        db: u8,
        pattern: Option<&str>,
        type_filter: Option<RedisType>,
        max_keys: usize,
    ) -> Result<Vec<KeyMeta>> {
        if !(1..=MAX_REDIS_SCAN_ALL_KEYS).contains(&max_keys) {
            return Err(DomainError::InvalidConfig(format!(
                "Redis scan_all 最大 key 数必须在 1–{MAX_REDIS_SCAN_ALL_KEYS} 之间"
            )));
        }
        let mut cursor = 0u64;
        let mut out: Vec<KeyMeta> = Vec::new();
        loop {
            let r = retry_idempotent_read!(
                config.id,
                self.driver.evict_pool(&config.id),
                self.driver
                    .scan(config, db, cursor, pattern, type_filter, 200)
                    .await
            )?;
            out.extend(r.keys);
            cursor = r.cursor;
            if cursor == 0 || out.len() >= max_keys {
                break;
            }
        }
        if out.len() > max_keys {
            out.truncate(max_keys);
        }
        tracing::info!(
            connection_id = %config.id,
            db,
            keys = out.len(),
            limited = out.len() >= max_keys,
            filtered = pattern.is_some() || type_filter.is_some(),
            "redis full scan completed"
        );
        Ok(out)
    }

    /// 执行一批增量扫描，返回游标供调用方继续。
    /// `pattern` 通过服务端 `MATCH` 过滤，避免拉取全库数据。
    pub async fn scan_batch(
        &self,
        config: &ConnectionConfig,
        db: u8,
        cursor: u64,
        pattern: Option<&str>,
        type_filter: Option<RedisType>,
        count: u32,
    ) -> Result<ScanResult> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver
                .scan(config, db, cursor, pattern, type_filter, count)
                .await
        )
    }

    pub async fn key_type(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
    ) -> Result<RedisType> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.key_type(config, db, key).await
        )
    }

    pub async fn key_ttl(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<i64> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.key_ttl(config, db, key).await
        )
    }

    pub async fn get_value(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
    ) -> Result<RedisValue> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.get_value(config, db, key).await
        )
    }

    pub async fn get_value_limited(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        limit: usize,
    ) -> Result<RedisValueLoad> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.get_value_limited(config, db, key, limit).await
        )
    }

    pub async fn db_size(&self, config: &ConnectionConfig, db: u8) -> Result<u64> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver.db_size(config, db).await
        )
    }

    /// 分段读取完整值，供导出使用。
    pub async fn read_value_page(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        kind: Option<RedisType>,
        cursor: ValuePageCursor,
        max_items: u32,
    ) -> Result<RedisValuePage> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver
                .read_value_page(config, db, key, kind, cursor.clone(), max_items)
                .await
        )
    }

    /// 导出首页的有界批量读取；结果顺序与 `keys` 完全一致。
    pub async fn read_value_first_pages(
        &self,
        config: &ConnectionConfig,
        db: u8,
        keys: &[String],
        max_items: u32,
    ) -> Result<Vec<RedisValuePage>> {
        retry_idempotent_read!(
            config.id,
            self.driver.evict_pool(&config.id),
            self.driver
                .read_value_first_pages(config, db, keys, max_items)
                .await
        )
    }

    /// 分段写入导入值；写操作不做断连重试。
    pub async fn write_value_items(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        items: &RedisValue,
    ) -> Result<u64> {
        let result = self.driver.write_value_items(config, db, key, items).await;
        match &result {
            Ok(written) => {
                tracing::info!(connection_id = %config.id, db, key_bytes = key.len(), written, "redis value items written")
            }
            Err(error) => {
                tracing::warn!(error = %error, connection_id = %config.id, db, key_bytes = key.len(), "write redis value items failed")
            }
        }
        result
    }

    pub fn is_write_command(&self, command: &str) -> bool {
        self.driver.is_write_command(command)
    }

    pub async fn delete_key(&self, config: &ConnectionConfig, db: u8, key: &str) -> Result<bool> {
        let result = self.driver.delete_key(config, db, key).await;
        match &result {
            Ok(deleted) => {
                tracing::info!(connection_id = %config.id, db, key_bytes = key.len(), deleted, "redis key delete completed")
            }
            Err(error) => {
                tracing::warn!(error = %error, connection_id = %config.id, db, key_bytes = key.len(), "delete redis key failed")
            }
        }
        result
    }

    pub async fn set_ttl(
        &self,
        config: &ConnectionConfig,
        db: u8,
        key: &str,
        ttl_secs: Option<i64>,
    ) -> Result<bool> {
        let result = self.driver.set_ttl(config, db, key, ttl_secs).await;
        match &result {
            Ok(changed) => {
                tracing::info!(connection_id = %config.id, db, key_bytes = key.len(), changed, persistent = ttl_secs.is_none(), "redis ttl update completed")
            }
            Err(error) => {
                tracing::warn!(error = %error, connection_id = %config.id, db, key_bytes = key.len(), "update redis ttl failed")
            }
        }
        result
    }

    pub async fn execute_command(
        &self,
        config: &ConnectionConfig,
        db: u8,
        argv: Vec<String>,
    ) -> Result<RedisValue> {
        let command = safe_command_name(argv.first().map(String::as_str));
        let argument_count = argv.len();
        let write = argv.first().is_some_and(|name| self.is_write_command(name));
        let result = self.driver.execute_command(config, db, argv).await;
        match &result {
            Ok(_) => {
                tracing::info!(connection_id = %config.id, db, command, argument_count, write, "redis command completed")
            }
            Err(error) => {
                tracing::warn!(error = %error, connection_id = %config.id, db, command, argument_count, write, "redis command failed")
            }
        }
        result
    }
}

fn safe_command_name(command: Option<&str>) -> String {
    command
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        .map_or_else(|| "invalid".into(), str::to_ascii_uppercase)
}

#[cfg(test)]
mod tests {
    use super::safe_command_name;

    #[test]
    fn redis_log_command_name_never_keeps_arguments_or_controls() {
        assert_eq!(safe_command_name(Some("get")), "GET");
        assert_eq!(safe_command_name(Some("AUTH secret")), "invalid");
        assert_eq!(safe_command_name(Some("GET\nsecret")), "invalid");
        assert_eq!(safe_command_name(None), "invalid");
    }
}
