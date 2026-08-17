//! SQL 连接服务，按配置的驱动类型路由操作。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ramag_domain::entities::{
    Column, ConnectionConfig, ConnectionId, DriverKind, ForeignKey, Index, Query, QueryResult,
    Schema, Table,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{CancelHandle, Driver, Storage};

mod history;

pub struct ConnectionService {
    drivers: HashMap<DriverKind, Arc<dyn Driver>>,
    storage: Arc<dyn Storage>,
    revision: AtomicU64,
}

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
        if let Err(error) = config.validate().map_err(DomainError::InvalidConfig) {
            tracing::warn!(
                operation = "connection_validate",
                error = %error,
                connection_id = %config.id,
                driver = ?config.driver,
                "connection validation failed"
            );
            return Err(error);
        }
        let driver = self
            .drivers
            .get(&config.driver)
            .ok_or_else(|| DomainError::InvalidConfig(format!("驱动不可用：{:?}", config.driver)));
        if let Err(error) = &driver {
            tracing::error!(
                operation = "connection_driver_lookup",
                error = %error,
                connection_id = %config.id,
                driver = ?config.driver,
                "connection driver unavailable"
            );
        }
        driver
    }

    /// 列出所有驱动类型的连接。
    pub async fn list(&self) -> Result<Vec<ConnectionConfig>> {
        let result = self.storage.list_connections().await;
        if let Err(error) = &result {
            tracing::error!(
                operation = "connection_list",
                error = %error,
                "list connections failed"
            );
        }
        result
    }

    pub async fn get(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        let result = self.storage.get_connection(id).await;
        if let Err(error) = &result {
            tracing::error!(
                operation = "connection_get",
                error = %error,
                connection_id = %id,
                "load connection failed"
            );
        }
        result
    }

    pub async fn save(&self, config: &ConnectionConfig) -> Result<()> {
        if let Err(error) = config.validate().map_err(DomainError::InvalidConfig) {
            tracing::warn!(
                operation = "connection_save_validate",
                error = %error,
                connection_id = %config.id,
                driver = ?config.driver,
                "connection validation failed before save"
            );
            return Err(error);
        }
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
            if let Err(error) = config.validate().map_err(DomainError::InvalidConfig) {
                tracing::warn!(
                    operation = "connection_import_validate",
                    error = %error,
                    connection_id = %config.id,
                    driver = ?config.driver,
                    "imported connection validation failed"
                );
                return Err(error);
            }
        }
        match self.storage.save_connections(configs).await {
            Ok(()) => {
                self.bump_revision();
                tracing::info!(
                    operation = "connection_import",
                    count = configs.len(),
                    "imported connections saved"
                );
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    operation = "connection_import",
                    error = %error,
                    count = configs.len(),
                    "save imported connections failed"
                );
                Err(error)
            }
        }
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
        // 清理历史与草稿，避免删除账号后残留敏感文本。
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
        let result = match self.driver_for(config) {
            Ok(driver) => driver.server_version(config).await,
            Err(error) => Err(error),
        };
        log_connection_result("sql_server_version", config, None, None, &result);
        result
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

    pub async fn list_schemas(&self, config: &ConnectionConfig) -> Result<Vec<Schema>> {
        let result = retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?.list_schemas(config).await
        );
        log_connection_result("sql_list_schemas", config, None, None, &result);
        result
    }

    pub async fn list_tables(&self, config: &ConnectionConfig, schema: &str) -> Result<Vec<Table>> {
        let result = retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?.list_tables(config, schema).await
        );
        log_connection_result("sql_list_tables", config, Some(schema), None, &result);
        result
    }

    pub async fn list_columns(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>> {
        let result = retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_columns(config, schema, table)
                .await
        );
        log_connection_result(
            "sql_list_columns",
            config,
            Some(schema),
            Some(table),
            &result,
        );
        result
    }

    pub async fn list_indexes(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>> {
        let result = retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_indexes(config, schema, table)
                .await
        );
        log_connection_result(
            "sql_list_indexes",
            config,
            Some(schema),
            Some(table),
            &result,
        );
        result
    }

    pub async fn list_foreign_keys(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>> {
        let result = retry_idempotent_read!(
            config.id,
            self.evict_pool(config),
            self.driver_for(config)?
                .list_foreign_keys(config, schema, table)
                .await
        );
        log_connection_result(
            "sql_list_foreign_keys",
            config,
            Some(schema),
            Some(table),
            &result,
        );
        result
    }

    pub async fn cancel_query(&self, config: &ConnectionConfig, thread_id: u64) -> Result<()> {
        let result = match self.driver_for(config) {
            Ok(driver) => driver.cancel_query(config, thread_id).await,
            Err(error) => Err(error),
        };
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
        let result = match self.driver_for(config) {
            Ok(driver) => driver.execute(config, query).await,
            Err(error) => Err(error),
        };
        log_query_result(config, query, &result, false);
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
        self.append_history(config, query, &result, false).await;
        result
    }
}

fn log_connection_result<T>(
    operation: &'static str,
    config: &ConnectionConfig,
    schema: Option<&str>,
    table: Option<&str>,
    result: &Result<T>,
) {
    if let Err(error) = result {
        tracing::warn!(
            operation,
            error = %error,
            connection_id = %config.id,
            driver = ?config.driver,
            schema = schema.unwrap_or("-"),
            table = table.unwrap_or("-"),
            "connection operation failed"
        );
    }
}

pub(super) fn log_query_result(
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
