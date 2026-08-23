//! SQL 数据库接口。

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;

use crate::entities::{
    Column, ConnectionConfig, ConnectionId, ForeignKey, Index, Query, QueryResult, Schema, Table,
    TransactionId,
};
use crate::error::Result;

/// 保存用于取消查询的后端会话 ID；`0` 表示尚未取得。
pub type CancelHandle = Arc<AtomicU64>;

#[async_trait]
pub trait Driver: Send + Sync {
    /// 用于日志 / UI 显示，如 "mysql"
    fn name(&self) -> &'static str;

    async fn test_connection(&self, config: &ConnectionConfig) -> Result<()>;

    /// 服务端版本，如 "8.0.32"
    async fn server_version(&self, _config: &ConnectionConfig) -> Result<String> {
        Err(crate::error::DomainError::NotImplemented(
            "server_version".into(),
        ))
    }

    async fn execute(&self, config: &ConnectionConfig, query: &Query) -> Result<QueryResult>;

    /// 默认不支持取消，直接调用 `execute`。
    async fn execute_cancellable(
        &self,
        config: &ConnectionConfig,
        query: &Query,
        _handle: CancelHandle,
    ) -> Result<QueryResult> {
        self.execute(config, query).await
    }

    /// Opens a driver-owned transaction session for the SQL query console.
    async fn begin_transaction(&self, _config: &ConnectionConfig) -> Result<TransactionId> {
        Err(crate::error::DomainError::NotImplemented(
            "begin_transaction".into(),
        ))
    }

    /// Executes SQL on a previously opened transaction without committing it.
    async fn execute_in_transaction(
        &self,
        _config: &ConnectionConfig,
        _transaction: &TransactionId,
        _query: &Query,
    ) -> Result<QueryResult> {
        Err(crate::error::DomainError::NotImplemented(
            "execute_in_transaction".into(),
        ))
    }

    /// Commits and closes a driver-owned transaction session.
    async fn commit_transaction(
        &self,
        _config: &ConnectionConfig,
        _transaction: &TransactionId,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "commit_transaction".into(),
        ))
    }

    /// Rolls back and closes a driver-owned transaction session.
    async fn rollback_transaction(
        &self,
        _config: &ConnectionConfig,
        _transaction: &TransactionId,
    ) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "rollback_transaction".into(),
        ))
    }

    /// 取消正在执行的查询。MySQL 走 `KILL QUERY`，PG 走 `pg_cancel_backend`
    async fn cancel_query(&self, _config: &ConnectionConfig, _thread_id: u64) -> Result<()> {
        Err(crate::error::DomainError::NotImplemented(
            "cancel_query".into(),
        ))
    }

    async fn list_schemas(&self, config: &ConnectionConfig) -> Result<Vec<Schema>>;

    async fn list_tables(&self, config: &ConnectionConfig, schema: &str) -> Result<Vec<Table>>;

    async fn list_columns(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>>;

    async fn list_indexes(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>>;

    async fn list_foreign_keys(
        &self,
        config: &ConnectionConfig,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>>;

    /// 配置变更后使对应连接池失效。
    fn evict_pool(&self, _id: &ConnectionId) {}
}
