//! PostgreSQL 驱动。实现 SqlBackend，并通过 `impl_driver_for!` 提供 Driver 接口。
//! 支持双引号标识符、取消查询、切换默认模式和美元引号字符串。

pub mod errors;
pub mod execute;
pub mod metadata;
pub mod pool;
pub mod types;

use async_trait::async_trait;

use ramag_domain::entities::{
    Column, ConnectionConfig, DriverKind, ForeignKey, Index, Schema, Table, Trigger, Value,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::CancelHandle;
use ramag_infra_sql_shared::SqlBackend;
use ramag_infra_sql_shared::sql::SplitOptions;
use ramag_infra_sql_shared::{PoolCache, TransactionStore};
use sqlx::postgres::{PgPool, PgQueryResult, PgRow, Postgres};
use sqlx::{Column as _, Row as _, TypeInfo as _};

/// 内部只持有共享连接池缓存，克隆不会复制连接池。
#[derive(Clone, Default)]
pub struct PostgresDriver {
    pools: PoolCache<Postgres>,
    transactions: TransactionStore<Postgres>,
}

impl PostgresDriver {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SqlBackend for PostgresDriver {
    type Db = Postgres;

    fn name(&self) -> &'static str {
        "postgres"
    }

    fn driver_kind(&self) -> DriverKind {
        DriverKind::Postgres
    }

    fn cache(&self) -> &PoolCache<Self::Db> {
        &self.pools
    }

    fn transaction_store(&self) -> &TransactionStore<Self::Db> {
        &self.transactions
    }

    fn quote_identifier(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn cancel_query_sql(&self, backend_id: u64) -> String {
        format!("SELECT pg_cancel_backend({backend_id})")
    }

    fn use_database_sql(&self, db: &str) -> Option<String> {
        // PostgreSQL 无法在现有连接中切换数据库，因此通过搜索路径切换默认模式。
        Some(format!(
            "SET search_path TO \"{}\"",
            db.replace('"', "\"\"")
        ))
    }

    fn split_options(&self) -> SplitOptions {
        SplitOptions::postgres()
    }

    async fn build_pool(&self, config: &ConnectionConfig) -> Result<PgPool> {
        pool::build_pool(config).await
    }

    fn decode_row(&self, row: &PgRow) -> Result<Vec<Value>> {
        types::decode_row(row)
    }

    fn extract_columns(&self, row: &PgRow) -> (Vec<String>, Vec<String>) {
        row.columns()
            .iter()
            .map(|c| (c.name().to_string(), c.type_info().name().to_string()))
            .unzip()
    }

    async fn extract_columns_fallback(
        &self,
        conn: &mut sqlx::postgres::PgConnection,
        sql: &str,
    ) -> Option<(Vec<String>, Vec<String>)> {
        execute::extract_columns_fallback(conn, sql).await
    }

    fn rows_affected(&self, qr: &PgQueryResult) -> u64 {
        qr.rows_affected()
    }

    async fn record_backend_id(
        &self,
        conn: &mut sqlx::pool::PoolConnection<Postgres>,
        handle: &CancelHandle,
    ) {
        execute::record_backend_id(conn, handle).await
    }

    fn map_database_error(&self, err: &sqlx::Error) -> Option<DomainError> {
        errors::map_postgres_database_error(err)
    }

    async fn server_version_impl(&self, pool: &PgPool) -> Result<String> {
        metadata::server_version(pool).await
    }

    async fn list_schemas_impl(&self, pool: &PgPool) -> Result<Vec<Schema>> {
        metadata::list_schemas(pool).await
    }

    async fn list_tables_impl(&self, pool: &PgPool, schema: &str) -> Result<Vec<Table>> {
        metadata::list_tables(pool, schema).await
    }

    async fn list_columns_impl(
        &self,
        pool: &PgPool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>> {
        metadata::list_columns(pool, schema, table).await
    }

    async fn list_indexes_impl(
        &self,
        pool: &PgPool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>> {
        metadata::list_indexes(pool, schema, table).await
    }

    async fn list_foreign_keys_impl(
        &self,
        pool: &PgPool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>> {
        metadata::list_foreign_keys(pool, schema, table).await
    }

    async fn list_triggers_impl(
        &self,
        pool: &PgPool,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Trigger>> {
        metadata::list_triggers(pool, schema, table).await
    }
}

ramag_infra_sql_shared::impl_driver_for!(PostgresDriver);
