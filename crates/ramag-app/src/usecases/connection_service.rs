//! ConnectionService：SQL 类多 driver 聚合，UI 持 `Arc<ConnectionService>` 即可。
//! 内部按 `config.driver` 路由到 `HashMap<DriverKind, Arc<dyn Driver>>`；Redis 走独立的 RedisService

use std::collections::HashMap;
use std::sync::Arc;

use ramag_domain::entities::{
    Column, ConnectionConfig, ConnectionId, DriverKind, ForeignKey, Index, Query, QueryHistoryPage,
    QueryRecord, QueryResult, Schema, Table,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{CancelHandle, Driver, Storage};

pub struct ConnectionService {
    drivers: HashMap<DriverKind, Arc<dyn Driver>>,
    storage: Arc<dyn Storage>,
}

const HISTORY_INLINE_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

impl ConnectionService {
    pub fn new(drivers: HashMap<DriverKind, Arc<dyn Driver>>, storage: Arc<dyn Storage>) -> Self {
        Self { drivers, storage }
    }

    /// 按 config.driver 取 driver；缺失返回 InvalidConfig
    fn driver_for(&self, config: &ConnectionConfig) -> Result<&Arc<dyn Driver>> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        self.drivers
            .get(&config.driver)
            .ok_or_else(|| DomainError::InvalidConfig(format!("驱动不可用: {:?}", config.driver)))
    }

    // 连接 CRUD（走 storage）

    /// 含全部 driver 的连接
    pub async fn list(&self) -> Result<Vec<ConnectionConfig>> {
        self.storage.list_connections().await
    }

    pub async fn save(&self, config: &ConnectionConfig) -> Result<()> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        self.storage.save_connection(config).await
    }

    pub async fn delete(&self, id: &ConnectionId) -> Result<()> {
        self.storage.delete_connection(id).await?;
        // 连接删除已由用户确认；同步清理不可再访问的查询历史与本地草稿，避免敏感文本残留。
        if let Err(e) = self.storage.clear_history(Some(id)).await {
            tracing::warn!(error = %e, connection_id = %id, "cleanup deleted connection history failed");
        }
        for key in [
            format!("sql_query_drafts_{id}"),
            format!("mongo_query_drafts_{id}"),
        ] {
            if let Err(e) = self.storage.delete_preference(&key).await {
                tracing::warn!(error = %e, connection_id = %id, "cleanup deleted connection drafts failed");
            }
        }
        Ok(())
    }

    // 连接动作（走 driver）

    pub async fn test(&self, config: &ConnectionConfig) -> Result<()> {
        self.driver_for(config)?.test_connection(config).await
    }

    pub async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        self.driver_for(config)?.server_version(config).await
    }

    /// 失效池缓存。用户改 config 后必须调，否则旧池按旧 host/db 工作
    pub fn evict_pool(&self, config: &ConnectionConfig) {
        if let Ok(driver) = self.driver_for(config) {
            driver.evict_pool(&config.id);
        }
    }

    /// 按连接 ID 清理全部 SQL 驱动池。
    ///
    /// 编辑连接时允许切换数据库类型；此时仅按新类型清理会遗留旧驱动池，
    /// 后续关闭标签也无法再从新配置推断旧类型。
    pub fn evict_all_pools(&self, id: &ConnectionId) {
        for driver in self.drivers.values() {
            driver.evict_pool(id);
        }
    }

    // 元数据查询（走 driver）。只读，故用 retry_idempotent_read! 兜底「查询执行到一半断连」
    // （sqlx test_before_acquire 已能在取连接时换掉死连接，这里再加一层重连重试）

    pub async fn list_schemas(&self, config: &ConnectionConfig) -> Result<Vec<Schema>> {
        retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?.list_schemas(config).await
        )
    }

    pub async fn list_tables(&self, config: &ConnectionConfig, schema: &str) -> Result<Vec<Table>> {
        retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?.list_tables(config, schema).await
        )
    }

    pub async fn list_columns(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>> {
        retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_columns(config, schema, table)
                .await
        )
    }

    pub async fn list_indexes(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>> {
        retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_indexes(config, schema, table)
                .await
        )
    }

    pub async fn list_foreign_keys(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>> {
        retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_foreign_keys(config, schema, table)
                .await
        )
    }

    // 查询执行

    pub async fn cancel_query(&self, config: &ConnectionConfig, thread_id: u64) -> Result<()> {
        self.driver_for(config)?
            .cancel_query(config, thread_id)
            .await
    }

    /// 可取消执行 + 写历史。driver 把后端 thread id 写入 handle，UI 另线程取出转交 cancel_query
    pub async fn execute_cancellable_with_history(
        &self,
        config: &ConnectionConfig,
        query: &Query,
        handle: CancelHandle,
    ) -> Result<QueryResult> {
        let result = match self.driver_for(config) {
            Ok(driver) => driver.execute_cancellable(config, query, handle).await,
            Err(e) => Err(e),
        };
        self.append_history_for(config, query, &result).await;
        result
    }

    pub async fn execute_with_history(
        &self,
        config: &ConnectionConfig,
        query: &Query,
    ) -> Result<QueryResult> {
        let result = match self.driver_for(config) {
            Ok(driver) => driver.execute(config, query).await,
            Err(e) => Err(e),
        };
        self.append_history_for(config, query, &result).await;
        result
    }

    /// 写历史失败仅 warn，不阻塞主流程
    async fn append_history_for(
        &self,
        config: &ConnectionConfig,
        query: &Query,
        result: &Result<QueryResult>,
    ) {
        let record = match result {
            Ok(r) => QueryRecord::new_success(
                config.id.clone(),
                &config.name,
                &query.sql,
                r.elapsed_ms,
                if r.rows.is_empty() {
                    r.affected_rows
                } else {
                    r.rows.len() as u64
                },
            ),
            Err(e) => {
                QueryRecord::new_failed(config.id.clone(), &config.name, &query.sql, e.to_string())
            }
        };
        if let Err(e) = self.storage.append_history(&record).await {
            tracing::warn!(error = %e, "append history failed");
        }
    }

    // 查询历史（走 storage）

    pub async fn list_history(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<QueryHistoryPage> {
        self.storage
            .list_history_bounded(connection_id, limit, HISTORY_INLINE_BYTE_BUDGET)
            .await
    }

    /// 删除单条查询历史（历史中心行删除按钮用）
    pub async fn delete_history(&self, id: &ramag_domain::entities::QueryRecordId) -> Result<()> {
        self.storage.delete_history(id).await
    }

    /// 清空某连接（None = 全部）的查询历史
    pub async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()> {
        self.storage.clear_history(connection_id).await
    }
}
