//! SQL driver 的共享抽象与执行模板。

mod query;
mod transaction;
use query::*;
use std::time::Instant;

pub use transaction::{
    MAX_SAVEPOINT_NAME_BYTES, begin_transaction_impl, commit_transaction_impl,
    create_savepoint_impl, execute_in_transaction_impl, release_savepoint_impl,
    rollback_to_savepoint_impl, rollback_transaction_impl,
};

use async_trait::async_trait;
use futures::TryStreamExt as _;
use ramag_domain::entities::{
    Column, ConnectionConfig, DriverKind, ForeignKey, Index, Query, QueryResult, Row, Schema,
    Table, Value, Warning,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::CancelHandle;
use sqlx::pool::PoolConnection;
use sqlx::{Acquire as _, Database, Executor, IntoArguments, Pool};
use tracing::{debug, info, warn};

use crate::errors::map_sqlx_common;
use crate::pool::PoolCache;
use crate::sql::{
    MAX_SQL_STATEMENTS, SplitOptions, first_keyword, inject_limit_if_needed,
    is_query_returning_rows, is_write_statement, split_statements_bounded, sql_has_no_limit_marker,
};
use crate::transaction::TransactionStore;

/// 单次查询保留的警告上限，包含可能的截断提示。
pub const MAX_QUERY_WARNINGS: usize = 1_000;
/// 超过该估算常驻内存后提示风险，但继续加载。
const QUERY_RESULT_MEMORY_WARNING_BYTES: u64 =
    ramag_domain::entities::INTERACTIVE_RESULT_WARNING_BYTES as u64;
/// 结果行与列元数据估算的常驻内存硬上限；单个在途驱动行不计入。
const MAX_QUERY_RESULT_BYTES: u64 = ramag_domain::entities::MAX_INTERACTIVE_RESULT_BYTES as u64;
const MAX_QUERY_RESULT_COLUMNS: usize = 4_096;
const MAX_QUERY_RESULT_METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// SQL driver 抽象；泛型约束适配 sqlx 0.8。
#[async_trait]
pub trait SqlBackend: Send + Sync + 'static
where
    for<'q> <Self::Db as Database>::Arguments<'q>: IntoArguments<'q, Self::Db>,
    for<'c> &'c Pool<Self::Db>: Executor<'c, Database = Self::Db>,
    for<'c> &'c mut <Self::Db as Database>::Connection: Executor<'c, Database = Self::Db>,
{
    type Db: Database;

    fn name(&self) -> &'static str;

    /// 共享池在缓存命中前也必须确认 driver 类型，不能只依赖具体 build_pool 的 miss 路径。
    fn driver_kind(&self) -> DriverKind;

    fn cache(&self) -> &PoolCache<Self::Db>;

    /// Long-lived transaction slots are kept beside the driver's pool cache.
    fn transaction_store(&self) -> &TransactionStore<Self::Db>;

    fn quote_identifier(&self, ident: &str) -> String;

    /// 取消语句：MySQL `KILL QUERY`，PG `pg_cancel_backend()`。
    fn cancel_query_sql(&self, backend_id: u64) -> String;

    /// 返回切换数据库语句；PG 在连接时绑定，故返回 None。
    fn use_database_sql(&self, db: &str) -> Option<String>;

    fn split_options(&self) -> SplitOptions;

    async fn build_pool(&self, config: &ConnectionConfig) -> Result<Pool<Self::Db>>;

    fn decode_row(&self, row: &<Self::Db as Database>::Row) -> Result<Vec<Value>>;

    fn extract_columns(&self, row: &<Self::Db as Database>::Row) -> (Vec<String>, Vec<String>);

    /// 空结果集的列定义；默认 None。
    async fn extract_columns_fallback(
        &self,
        _conn: &mut <Self::Db as Database>::Connection,
        _sql: &str,
    ) -> Option<(Vec<String>, Vec<String>)> {
        None
    }

    fn rows_affected(&self, query_result: &<Self::Db as Database>::QueryResult) -> u64;

    /// 将后端会话 ID 写入取消句柄。
    async fn record_backend_id(
        &self,
        _conn: &mut PoolConnection<Self::Db>,
        _handle: &CancelHandle,
    ) {
    }

    /// Prepares a pooled connection for one-shot execution. Interactive
    /// transactions intentionally bypass this hook and keep their own state.
    async fn prepare_for_auto_commit(
        &self,
        _conn: &mut <Self::Db as Database>::Connection,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }

    async fn fetch_warnings(&self, _conn: &mut <Self::Db as Database>::Connection) -> Vec<Warning> {
        Vec::new()
    }

    /// 按数据库错误码映射错误；优先于通用映射。
    fn map_database_error(&self, _err: &sqlx::Error) -> Option<DomainError> {
        None
    }

    async fn server_version_impl(&self, pool: &Pool<Self::Db>) -> Result<String>;

    async fn list_schemas_impl(&self, pool: &Pool<Self::Db>) -> Result<Vec<Schema>>;

    async fn list_tables_impl(&self, pool: &Pool<Self::Db>, schema: &str) -> Result<Vec<Table>>;

    async fn list_columns_impl(
        &self,
        pool: &Pool<Self::Db>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Column>>;

    async fn list_indexes_impl(
        &self,
        pool: &Pool<Self::Db>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<Index>>;

    async fn list_foreign_keys_impl(
        &self,
        pool: &Pool<Self::Db>,
        schema: &str,
        table: &str,
    ) -> Result<Vec<ForeignKey>>;
}

async fn get_pool<B>(b: &B, config: &ConnectionConfig) -> Result<Pool<B::Db>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_backend_config(config, b.driver_kind(), b.name())?;
    // 固定请求代际，避免旧请求命中或写回新配置的连接池。
    let generation = b.cache().generation_for_request(&config.id);
    if let Some(p) = b.cache().get(&config.id, generation) {
        return Ok(p);
    }
    let build_lock = b.cache().build_lock(&config.id, generation);
    let _guard = build_lock.lock().await;
    if !b.cache().is_current_generation(&config.id, generation) {
        return b.build_pool(config).await;
    }
    // 等锁期间可能已有请求完成建池。
    if let Some(p) = b.cache().get(&config.id, generation) {
        return Ok(p);
    }
    let pool = b.build_pool(config).await?;
    b.cache()
        .insert(config.id.clone(), generation, pool.clone());
    Ok(pool)
}

fn validate_backend_config(
    config: &ConnectionConfig,
    expected: DriverKind,
    backend_name: &str,
) -> Result<()> {
    config.validate().map_err(DomainError::InvalidConfig)?;
    if config.driver != expected {
        return Err(DomainError::InvalidConfig(format!(
            "{backend_name} 不支持 {:?} 类型连接",
            config.driver
        )));
    }
    Ok(())
}

fn map_err<B>(b: &B, err: sqlx::Error) -> DomainError
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    b.map_database_error(&err)
        .unwrap_or_else(|| map_sqlx_common(&err))
}

pub async fn test_connection_impl<B>(b: &B, config: &ConnectionConfig) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let pool = get_pool(b, config).await?;
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|e| map_err(b, e))?;
    Ok(())
}

pub async fn server_version_impl<B>(b: &B, config: &ConnectionConfig) -> Result<String>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let pool = get_pool(b, config).await?;
    b.server_version_impl(&pool).await
}

pub async fn list_schemas_impl<B>(b: &B, config: &ConnectionConfig) -> Result<Vec<Schema>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let pool = get_pool(b, config).await?;
    b.list_schemas_impl(&pool).await
}

pub async fn list_tables_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    schema: &str,
) -> Result<Vec<Table>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_metadata_identifier(schema, "schema")?;
    let pool = get_pool(b, config).await?;
    b.list_tables_impl(&pool, schema).await
}

pub async fn list_columns_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    schema: &str,
    table: &str,
) -> Result<Vec<Column>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_metadata_identifier(schema, "schema")?;
    validate_metadata_identifier(table, "table")?;
    let pool = get_pool(b, config).await?;
    b.list_columns_impl(&pool, schema, table).await
}

pub async fn list_indexes_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    schema: &str,
    table: &str,
) -> Result<Vec<Index>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_metadata_identifier(schema, "schema")?;
    validate_metadata_identifier(table, "table")?;
    let pool = get_pool(b, config).await?;
    b.list_indexes_impl(&pool, schema, table).await
}

pub async fn list_foreign_keys_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_metadata_identifier(schema, "schema")?;
    validate_metadata_identifier(table, "table")?;
    let pool = get_pool(b, config).await?;
    b.list_foreign_keys_impl(&pool, schema, table).await
}

fn validate_metadata_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty() {
        return Err(DomainError::InvalidConfig(format!("{label} 不能为空")));
    }
    if value.len() > ramag_domain::entities::MAX_CONNECTION_IDENTIFIER_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DomainError::InvalidConfig(format!(
            "{label} 超过 {} KiB 上限或包含控制字符",
            ramag_domain::entities::MAX_CONNECTION_IDENTIFIER_BYTES / 1024
        )));
    }
    Ok(())
}

pub async fn cancel_query_impl<B>(b: &B, config: &ConnectionConfig, backend_id: u64) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let pool = get_pool(b, config).await?;
    let sql = b.cancel_query_sql(backend_id);
    sqlx::query(&sql)
        .execute(&pool)
        .await
        .map_err(|e| map_err(b, e))?;
    Ok(())
}

/// 执行多语句查询并管理 LIMIT、取消和警告。
pub async fn execute_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    query: &Query,
    handle: Option<CancelHandle>,
) -> Result<QueryResult>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    query.validate()?;
    let start = Instant::now();
    let pool = get_pool(b, config).await?;
    let mut conn: PoolConnection<B::Db> = pool.acquire().await.map_err(|e| map_err(b, e))?;

    b.prepare_for_auto_commit(&mut conn)
        .await
        .map_err(|e| map_err(b, e))?;

    if let Some(h) = &handle {
        b.record_backend_id(&mut conn, h).await;
    }

    if let Some(schema) = query.default_schema.as_deref().filter(|s| !s.is_empty())
        && let Some(use_sql) = b.use_database_sql(schema)
    {
        debug!(
            operation = "sql_query_switch_schema",
            schema, "switching default schema before query"
        );
        // MySQL 的 USE 不支持 prepared statement，需走简单查询。
        conn.execute(use_sql.as_str())
            .await
            .map_err(|e| map_err(b, e))?;
    }

    let statements = split_statements_bounded(&query.sql, b.split_options(), MAX_SQL_STATEMENTS)?;
    if statements.is_empty() {
        return Ok(QueryResult {
            columns: Vec::new(),
            column_types: Vec::new(),
            rows: Vec::new(),
            affected_rows: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
            warnings: Vec::new(),
            truncated: false,
        });
    }

    // 生产模式中任一写语句都会拒绝整批执行；细节仅记日志。
    if config.production
        && let Some(stmt) = statements.iter().find(|s| is_write_statement(s))
    {
        warn!(
            operation = "sql_query_read_only_guard",
            conn = %config.name,
            keyword = first_keyword(stmt).as_deref().unwrap_or("?"),
            "read-only write blocked"
        );
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }

    let last_idx = statements.len() - 1;
    let mut total_affected: u64 = 0;
    let mut accumulated_warnings: Vec<Warning> = Vec::new();
    let mut last_result = QueryResult {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        affected_rows: 0,
        elapsed_ms: 0,
        warnings: Vec::new(),
        truncated: false,
    };

    let user_disabled_limit = sql_has_no_limit_marker(&query.sql);
    if query.transactional {
        let mut transaction = conn.begin().await.map_err(|e| map_err(b, e))?;
        execute_statements(
            b,
            &mut transaction,
            query,
            &statements,
            last_idx,
            user_disabled_limit,
            &mut total_affected,
            &mut accumulated_warnings,
            &mut last_result,
        )
        .await?;
        transaction.commit().await.map_err(|e| map_err(b, e))?;
    } else {
        execute_statements(
            b,
            &mut conn,
            query,
            &statements,
            last_idx,
            user_disabled_limit,
            &mut total_affected,
            &mut accumulated_warnings,
            &mut last_result,
        )
        .await?;
    }

    if last_result.rows.is_empty() && last_result.columns.is_empty() {
        last_result.affected_rows = total_affected;
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    info!(
        operation = "sql_query",
        elapsed_ms,
        rows = last_result.rows.len(),
        affected = last_result.affected_rows,
        statements = statements.len(),
        "query completed"
    );

    Ok(QueryResult {
        elapsed_ms,
        warnings: accumulated_warnings,
        ..last_result
    })
}

fn is_transaction_control_statement(statement: &str) -> bool {
    matches!(
        first_keyword(statement).as_deref(),
        Some("BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE")
    )
}

#[allow(clippy::too_many_arguments)]
async fn execute_statements<B>(
    b: &B,
    conn: &mut <B::Db as Database>::Connection,
    query: &Query,
    statements: &[String],
    last_idx: usize,
    user_disabled_limit: bool,
    total_affected: &mut u64,
    accumulated_warnings: &mut Vec<Warning>,
    last_result: &mut QueryResult,
) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    for (i, stmt) in statements.iter().enumerate() {
        let trimmed = stmt.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let is_select = is_query_returning_rows(trimmed);
        let injected = if is_select && !user_disabled_limit {
            inject_limit_if_needed(trimmed, query.auto_limit)
        } else {
            None
        };
        let effective_sql: &str = injected.as_deref().unwrap_or(stmt.as_str());
        let mut result = if is_select {
            let max_result_bytes = query
                .result_byte_limit
                .map_or(MAX_QUERY_RESULT_BYTES, |limit| {
                    u64::try_from(limit).unwrap_or(MAX_QUERY_RESULT_BYTES)
                });
            run_select::<B>(b, conn, effective_sql, max_result_bytes).await?
        } else {
            run_dml::<B>(b, conn, effective_sql).await?
        };
        if !is_select {
            *total_affected = total_affected.saturating_add(result.affected_rows);
        }
        append_warnings_bounded(accumulated_warnings, std::mem::take(&mut result.warnings));
        append_warnings_bounded(accumulated_warnings, b.fetch_warnings(conn).await);
        if i == last_idx {
            *last_result = result;
        }
    }
    Ok(())
}

fn append_warnings_bounded(accumulated: &mut Vec<Warning>, incoming: Vec<Warning>) {
    if incoming.is_empty() {
        return;
    }
    let remaining = MAX_QUERY_WARNINGS.saturating_sub(accumulated.len());
    if incoming.len() <= remaining {
        accumulated.extend(incoming);
        return;
    }

    // 为截断提示预留一格。
    if remaining == 0 {
        accumulated.pop();
    } else {
        accumulated.extend(incoming.into_iter().take(remaining.saturating_sub(1)));
    }
    accumulated.push(Warning {
        level: "Client".into(),
        code: 0,
        message: format!(
            "警告数量超过 {MAX_QUERY_WARNINGS} 条，仅保留前 {} 条以控制资源占用",
            MAX_QUERY_WARNINGS - 1
        ),
    });
}

#[cfg(test)]
mod tests;
