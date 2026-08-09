//! SQL 连接服务，按配置的驱动类型路由操作。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ramag_domain::entities::{
    Column, ConnectionConfig, ConnectionId, DriverKind, ForeignKey, Index, Query, QueryHistoryPage,
    QueryRecord, QueryResult, Schema, Table,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{CancelHandle, Driver, Storage};

pub struct ConnectionService {
    drivers: HashMap<DriverKind, Arc<dyn Driver>>,
    storage: Arc<dyn Storage>,
    revision: AtomicU64,
}

const HISTORY_INLINE_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

impl ConnectionService {
    pub fn new(drivers: HashMap<DriverKind, Arc<dyn Driver>>, storage: Arc<dyn Storage>) -> Self {
        Self {
            drivers,
            storage,
            revision: AtomicU64::new(0),
        }
    }

    /// 获取配置对应的驱动。
    fn driver_for(&self, config: &ConnectionConfig) -> Result<&Arc<dyn Driver>> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        self.drivers
            .get(&config.driver)
            .ok_or_else(|| DomainError::InvalidConfig(format!("驱动不可用：{:?}", config.driver)))
    }

    /// 列出所有驱动类型的连接。
    pub async fn list(&self) -> Result<Vec<ConnectionConfig>> {
        self.storage.list_connections().await
    }

    pub async fn get(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        self.storage.get_connection(id).await
    }

    pub async fn save(&self, config: &ConnectionConfig) -> Result<()> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        if let Err(error) = self.storage.save_connection(config).await {
            tracing::error!(
                operation = "connection_save",
                error = %error,
                connection_id = %config.id,
                driver = ?config.driver,
                "save connection failed"
            );
            return Err(error);
        }
        self.bump_revision();
        tracing::info!(
            operation = "connection_save",
            connection_id = %config.id,
            driver = ?config.driver,
            "connection saved"
        );
        Ok(())
    }

    /// 原子保存一批连接；用于配置导入，避免中途失败留下半份结果。
    pub async fn save_many(&self, configs: &[ConnectionConfig]) -> Result<()> {
        if configs.is_empty() {
            return Ok(());
        }
        for config in configs {
            config.validate().map_err(DomainError::InvalidConfig)?;
        }
        if let Err(error) = self.storage.save_connections(configs).await {
            tracing::error!(
                operation = "connection_import",
                error = %error,
                count = configs.len(),
                "save imported connections failed"
            );
            return Err(error);
        }
        self.bump_revision();
        tracing::info!(
            operation = "connection_import",
            count = configs.len(),
            "imported connections saved"
        );
        Ok(())
    }

    pub async fn delete(&self, id: &ConnectionId) -> Result<()> {
        if let Err(error) = self.storage.delete_connection(id).await {
            tracing::error!(
                operation = "connection_delete",
                error = %error,
                connection_id = %id,
                "delete connection failed"
            );
            return Err(error);
        }
        // 连接删除已由用户确认；同步清理不可再访问的查询历史与本地草稿，避免敏感文本残留。
        if let Err(e) = self.storage.clear_history(Some(id)).await {
            tracing::warn!(
                operation = "connection_delete_cleanup",
                error = %e,
                connection_id = %id,
                resource = "query_history",
                "cleanup deleted connection history failed"
            );
        }
        for key in [
            format!("sql_query_drafts_{id}"),
            format!("mongo_query_drafts_{id}"),
        ] {
            if let Err(e) = self.storage.delete_preference(&key).await {
                tracing::warn!(
                    operation = "connection_delete_cleanup",
                    error = %e,
                    connection_id = %id,
                    resource = "query_draft",
                    "cleanup deleted connection drafts failed"
                );
            }
        }
        self.bump_revision();
        tracing::info!(operation = "connection_delete", connection_id = %id, "connection deleted");
        Ok(())
    }

    /// 连接配置变更修订号，供长期存活的页面发现其它入口完成的导入或编辑。
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub async fn test(&self, config: &ConnectionConfig) -> Result<()> {
        let started = std::time::Instant::now();
        let result = match self.driver_for(config) {
            Ok(driver) => driver.test_connection(config).await,
            Err(error) => Err(error),
        };
        match &result {
            Ok(()) => {
                tracing::info!(
                    operation = "connection_test",
                    connection_id = %config.id,
                    driver = ?config.driver,
                    elapsed_ms = started.elapsed().as_millis(),
                    "connection test succeeded"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "connection_test",
                    error = %error,
                    connection_id = %config.id,
                    driver = ?config.driver,
                    elapsed_ms = started.elapsed().as_millis(),
                    "connection test failed"
                )
            }
        }
        result
    }

    pub async fn server_version(&self, config: &ConnectionConfig) -> Result<String> {
        self.driver_for(config)?.server_version(config).await
    }

    /// 清除连接池缓存，避免配置修改后继续使用旧连接。
    pub fn evict_pool(&self, config: &ConnectionConfig) {
        if let Ok(driver) = self.driver_for(config) {
            driver.evict_pool(&config.id);
        }
    }

    /// 按连接 ID 清理全部 SQL 驱动池。
    /// 编辑连接时允许切换数据库类型；此时仅按新类型清理会遗留旧驱动池，
    /// 后续关闭标签也无法再从新配置推断旧类型。
    pub fn evict_all_pools(&self, id: &ConnectionId) {
        for driver in self.drivers.values() {
            driver.evict_pool(id);
        }
    }

    // 元数据查询是幂等读，连接中断时允许清除缓存并重试一次。

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

    pub async fn cancel_query(&self, config: &ConnectionConfig, thread_id: u64) -> Result<()> {
        let result = self
            .driver_for(config)?
            .cancel_query(config, thread_id)
            .await;
        match &result {
            Ok(()) => {
                tracing::info!(
                    operation = "sql_query_cancel",
                    connection_id = %config.id,
                    thread_id,
                    "query cancellation requested"
                )
            }
            Err(error) => {
                tracing::warn!(
                    operation = "sql_query_cancel",
                    error = %error,
                    connection_id = %config.id,
                    thread_id,
                    "query cancellation failed"
                )
            }
        }
        result
    }

    /// 执行可取消查询并写入历史；驱动通过句柄传递后端线程 ID。
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
        log_query_result(config, query, &result, true);
        self.append_history_for(config, query, &result).await;
        result
    }

    /// 执行但不写历史，供数据导入等批量场景使用。
    pub async fn execute(&self, config: &ConnectionConfig, query: &Query) -> Result<QueryResult> {
        self.driver_for(config)?.execute(config, query).await
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
        log_query_result(config, query, &result, false);
        self.append_history_for(config, query, &result).await;
        result
    }

    /// 历史写入失败仅记录警告，不阻塞查询。
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
            tracing::warn!(
                operation = "sql_query_history_append",
                error = %e,
                connection_id = %config.id,
                query_bytes = query.sql.len(),
                "append query history failed"
            );
        }
    }

    pub async fn list_history(
        &self,
        connection_id: Option<&ConnectionId>,
        limit: usize,
    ) -> Result<QueryHistoryPage> {
        self.storage
            .list_history_bounded(connection_id, limit, HISTORY_INLINE_BYTE_BUDGET)
            .await
    }

    pub async fn delete_history(&self, id: &ramag_domain::entities::QueryRecordId) -> Result<()> {
        self.storage.delete_history(id).await
    }

    /// 清空指定连接的查询历史；`None` 表示全部连接。
    pub async fn clear_history(&self, connection_id: Option<&ConnectionId>) -> Result<()> {
        self.storage.clear_history(connection_id).await
    }
}

fn log_query_result(
    config: &ConnectionConfig,
    query: &Query,
    result: &Result<QueryResult>,
    cancellable: bool,
) {
    match result {
        Ok(output) => tracing::info!(
            operation = "sql_query",
            connection_id = %config.id,
            driver = ?config.driver,
            query_bytes = query.sql.len(),
            rows = output.rows.len(),
            affected_rows = output.affected_rows,
            warnings = output.warnings.len(),
            truncated = output.truncated,
            elapsed_ms = output.elapsed_ms,
            cancellable,
            "query completed"
        ),
        Err(error) => tracing::warn!(
            operation = "sql_query",
            error = %error,
            connection_id = %config.id,
            driver = ?config.driver,
            query_bytes = query.sql.len(),
            cancellable,
            "query failed"
        ),
    }
}
