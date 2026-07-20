//! SqlBackend：SQL 类 driver 唯一抽象层 + 泛型模板（test/execute/cancel/list_*）。
//! driver crate 仅实现方言方法 + 行解码 + 元数据 SQL，由 `impl_driver_for!` 宏代理到 Driver

use std::time::Instant;

use async_trait::async_trait;
use futures::TryStreamExt as _;
use ramag_domain::entities::{
    Column, ConnectionConfig, DriverKind, ForeignKey, Index, Query, QueryResult, Row, Schema,
    Table, Value, Warning,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::CancelHandle;
use sqlx::pool::PoolConnection;
use sqlx::{Database, Executor, IntoArguments, Pool};
use tracing::{debug, info, warn};

use crate::errors::map_sqlx_common;
use crate::pool::PoolCache;
use crate::sql::{
    MAX_SQL_STATEMENTS, SplitOptions, first_keyword, inject_limit_if_needed,
    is_query_returning_rows, is_write_statement, split_statements_bounded, sql_has_no_limit_marker,
};

/// 单次查询保留的警告上限，包含可能的截断提示。
pub const MAX_QUERY_WARNINGS: usize = 1_000;
/// 超过该估算常驻内存后提示风险，但继续加载。
const QUERY_RESULT_MEMORY_WARNING_BYTES: u64 = 128 * 1024 * 1024;
/// 结果行与列元数据估算的常驻内存硬上限；单个在途驱动行不计入。
const MAX_QUERY_RESULT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_QUERY_RESULT_COLUMNS: usize = 4_096;
const MAX_QUERY_RESULT_METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// SQL 类 driver 抽象。`Db` 绑到 sqlx Database（MySql/Postgres/Sqlite 等）。
/// where 子句的 HRTB GAT 是 sqlx 0.8 必备，sqlx 内置 Database 自动满足
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

    /// 按 ConnectionId 缓存的连接池
    fn cache(&self) -> &PoolCache<Self::Db>;

    // 方言

    /// MySQL 反引号 / PG 双引号
    fn quote_identifier(&self, ident: &str) -> String;

    /// MySQL `KILL QUERY` / PG `pg_cancel_backend()`
    fn cancel_query_sql(&self, backend_id: u64) -> String;

    /// MySQL `USE <db>`；PG None（连接时绑定 db）
    fn use_database_sql(&self, db: &str) -> Option<String>;

    /// PG 需识别 dollar-quoted
    fn split_options(&self) -> SplitOptions;

    // 连接 / 池

    async fn build_pool(&self, config: &ConnectionConfig) -> Result<Pool<Self::Db>>;

    // 行解码

    fn decode_row(&self, row: &<Self::Db as Database>::Row) -> Result<Vec<Value>>;

    /// 列名 + 列类型名
    fn extract_columns(&self, row: &<Self::Db as Database>::Row) -> (Vec<String>, Vec<String>);

    /// 空结果集 fallback 列定义。默认 None，MySQL 走 `Connection::describe`
    async fn extract_columns_fallback(
        &self,
        _conn: &mut <Self::Db as Database>::Connection,
        _sql: &str,
    ) -> Option<(Vec<String>, Vec<String>)> {
        None
    }

    /// DML 受影响行数。sqlx 没抽到 trait 上，只能 hook
    fn rows_affected(&self, query_result: &<Self::Db as Database>::QueryResult) -> u64;

    /// 把后端 thread/session id 写入 cancel handle
    async fn record_backend_id(
        &self,
        _conn: &mut PoolConnection<Self::Db>,
        _handle: &CancelHandle,
    ) {
    }

    /// MySQL SHOW WARNINGS；其他 DB 默认空
    async fn fetch_warnings(&self, _conn: &mut PoolConnection<Self::Db>) -> Vec<Warning> {
        Vec::new()
    }

    /// 数据库错误码识别，优先于通用大类映射
    fn map_database_error(&self, _err: &sqlx::Error) -> Option<DomainError> {
        None
    }

    // 元数据 SQL（per-DB 实现，签名通用）

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

/// 取连接池：命中缓存即返，否则 build_pool 后 insert
async fn get_pool<B>(b: &B, config: &ConnectionConfig) -> Result<Pool<B::Db>>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    validate_backend_config(config, b.driver_kind(), b.name())?;
    // 在等待建池锁前固定请求代际；若期间配置被 evict，本次旧请求可完成，
    // 但不得命中新配置的池，也不得把旧池重新写回缓存。
    let generation = b.cache().generation_for_request(&config.id);
    if let Some(p) = b.cache().get(&config.id, generation) {
        return Ok(p);
    }
    let build_lock = b.cache().build_lock(&config.id, generation);
    let _guard = build_lock.lock().await;
    if !b.cache().is_current_generation(&config.id, generation) {
        return b.build_pool(config).await;
    }
    // 等锁期间其它请求可能已完成建池。
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

/// 先走 driver 自定义识别，未命中走 sqlx 通用大类
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

// 模板函数：由 `impl_driver_for!` 代理调用

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

/// 多语句切分 + LIMIT 注入 + cancel handle + warnings
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

    if let Some(h) = &handle {
        b.record_backend_id(&mut conn, h).await;
    }

    if let Some(schema) = query.default_schema.as_deref().filter(|s| !s.is_empty())
        && let Some(use_sql) = b.use_database_sql(schema)
    {
        debug!(?use_sql, "switching default schema before query");
        // MySQL `USE <db>` 在 prepared statement 协议不支持，必须走 COM_QUERY 简单查询
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
        });
    }

    // 生产模式只读保护：任一语句为写即整批拒绝，不执行其中任何一条。
    // 详细拦截信息进日志，页面只回统一文案
    if config.production
        && let Some(stmt) = statements.iter().find(|s| is_write_statement(s))
    {
        warn!(
            conn = %config.name,
            keyword = first_keyword(stmt).as_deref().unwrap_or("?"),
            "read-only mode: blocked write statement"
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
    };

    let user_disabled_limit = sql_has_no_limit_marker(&query.sql);

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

        let mut r = if is_select {
            run_select::<B>(b, &mut *conn, effective_sql).await?
        } else {
            run_dml::<B>(b, &mut *conn, effective_sql).await?
        };
        if !is_select {
            total_affected = total_affected.saturating_add(r.affected_rows);
        }
        append_warnings_bounded(&mut accumulated_warnings, std::mem::take(&mut r.warnings));
        append_warnings_bounded(&mut accumulated_warnings, b.fetch_warnings(&mut conn).await);
        if i == last_idx {
            last_result = r;
        }
    }

    if last_result.rows.is_empty() && last_result.columns.is_empty() {
        last_result.affected_rows = total_affected;
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    info!(
        elapsed_ms,
        rows = last_result.rows.len(),
        affected = last_result.affected_rows,
        statements = statements.len(),
        "query executed"
    );

    Ok(QueryResult {
        elapsed_ms,
        warnings: accumulated_warnings,
        ..last_result
    })
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

    // 为明确的截断提示预留一格；若此前恰好装满，则替换最后一条。
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

async fn run_select<B>(
    b: &B,
    conn: &mut <B::Db as Database>::Connection,
    sql: &str,
) -> Result<QueryResult>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let mut columns = Vec::new();
    let mut column_types = Vec::new();
    let mut domain_rows = Vec::new();
    let mut retained_bytes = 0u64;
    let mut limit_reached = None;
    let mut saw_row = false;

    {
        let mut rows = sqlx::query(sql).fetch(&mut *conn);
        while let Some(row) = rows.try_next().await.map_err(|e| map_err(b, e))? {
            if !saw_row {
                (columns, column_types) = b.extract_columns(&row);
                retained_bytes = validate_query_columns(&columns, &column_types)?;
                saw_row = true;
            }
            let row = Row {
                values: b.decode_row(&row)?,
            };
            if let Err(limit) = try_push_query_row(
                &mut domain_rows,
                &mut retained_bytes,
                row,
                MAX_QUERY_RESULT_BYTES,
            ) {
                limit_reached = Some(limit);
                break;
            }
        }
    }

    if !saw_row {
        (columns, column_types) = b
            .extract_columns_fallback(conn, sql)
            .await
            .unwrap_or_default();
        validate_query_columns(&columns, &column_types)?;
    }

    let warnings =
        query_result_memory_warning(retained_bytes, limit_reached.is_some(), domain_rows.len())
            .into_iter()
            .collect();

    Ok(QueryResult {
        columns,
        column_types,
        rows: domain_rows,
        affected_rows: 0,
        elapsed_ms: 0,
        warnings,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryResultLimit {
    Bytes,
}

fn query_result_memory_warning(
    retained_bytes: u64,
    truncated: bool,
    retained_rows: usize,
) -> Option<Warning> {
    let warning_mib = QUERY_RESULT_MEMORY_WARNING_BYTES / (1024 * 1024);
    let maximum_mib = MAX_QUERY_RESULT_BYTES / (1024 * 1024);
    let message = if truncated {
        format!(
            "查询结果超过客户端硬上限（{maximum_mib} MiB 常驻内存），仅保留前 {retained_rows} 行（已截断）；请增加 WHERE 或 LIMIT 缩小范围"
        )
    } else if retained_bytes >= QUERY_RESULT_MEMORY_WARNING_BYTES {
        format!(
            "查询结果已达到客户端内存提示线（{warning_mib} MiB 常驻内存）；本次结果未截断，达到 {maximum_mib} MiB 时将停止加载"
        )
    } else {
        return None;
    };
    Some(Warning {
        level: "Client".into(),
        code: 0,
        message,
    })
}

fn validate_query_columns(columns: &[String], column_types: &[String]) -> Result<u64> {
    validate_query_columns_with_limits(
        columns,
        column_types,
        MAX_QUERY_RESULT_COLUMNS,
        MAX_QUERY_RESULT_METADATA_BYTES,
    )
}

fn validate_query_columns_with_limits(
    columns: &[String],
    column_types: &[String],
    max_columns: usize,
    max_bytes: u64,
) -> Result<u64> {
    if columns.len() != column_types.len() {
        return Err(DomainError::QueryFailed(format!(
            "查询结果列名与类型数量不一致：{} != {}",
            columns.len(),
            column_types.len()
        )));
    }
    if columns.len() > max_columns {
        return Err(DomainError::QueryFailed(format!(
            "查询结果包含 {} 列，超过 {max_columns} 列安全上限；请减少 SELECT 字段",
            columns.len()
        )));
    }
    let retained = columns
        .iter()
        .chain(column_types)
        .try_fold(0u64, |total, value| {
            total.checked_add(u64::try_from(value.len()).unwrap_or(u64::MAX))
        })
        .ok_or_else(|| DomainError::QueryFailed("查询结果列元数据大小溢出".into()))?;
    if retained > max_bytes {
        return Err(DomainError::QueryFailed(format!(
            "查询结果列元数据超过 {} MiB 安全上限；请减少 SELECT 字段",
            max_bytes / (1024 * 1024)
        )));
    }
    Ok(retained)
}

fn try_push_query_row(
    rows: &mut Vec<Row>,
    retained_bytes: &mut u64,
    row: Row,
    max_bytes: u64,
) -> std::result::Result<(), QueryResultLimit> {
    let next_bytes = retained_bytes
        .checked_add(row.retained_bytes())
        .ok_or(QueryResultLimit::Bytes)?;
    if next_bytes > max_bytes {
        return Err(QueryResultLimit::Bytes);
    }
    rows.push(row);
    *retained_bytes = next_bytes;
    Ok(())
}

async fn run_dml<B>(
    b: &B,
    conn: &mut <B::Db as Database>::Connection,
    sql: &str,
) -> Result<QueryResult>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let result = sqlx::query(sql)
        .execute(&mut *conn)
        .await
        .map_err(|e| map_err(b, e))?;
    Ok(QueryResult {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        affected_rows: b.rows_affected(&result),
        elapsed_ms: 0,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_QUERY_RESULT_BYTES, MAX_QUERY_WARNINGS, QUERY_RESULT_MEMORY_WARNING_BYTES,
        QueryResultLimit, append_warnings_bounded, query_result_memory_warning, try_push_query_row,
        validate_backend_config, validate_metadata_identifier, validate_query_columns_with_limits,
    };
    use ramag_domain::entities::{ConnectionConfig, DriverKind, Row, Value, Warning};

    fn warnings(count: usize) -> Vec<Warning> {
        (0..count)
            .map(|index| Warning {
                level: "Warning".into(),
                code: index as u32,
                message: format!("warning {index}"),
            })
            .collect()
    }

    #[test]
    fn warning_budget_keeps_exact_boundary() {
        let mut accumulated = Vec::new();
        append_warnings_bounded(&mut accumulated, warnings(MAX_QUERY_WARNINGS));

        assert_eq!(accumulated.len(), MAX_QUERY_WARNINGS);
        assert_ne!(
            accumulated.last().map(|warning| warning.level.as_str()),
            Some("Client")
        );
    }

    #[test]
    fn warning_budget_replaces_tail_with_truncation_marker() {
        let mut accumulated = warnings(MAX_QUERY_WARNINGS);
        append_warnings_bounded(&mut accumulated, warnings(1));

        assert_eq!(accumulated.len(), MAX_QUERY_WARNINGS);
        assert_eq!(
            accumulated.last().map(|warning| warning.level.as_str()),
            Some("Client")
        );
        assert_eq!(accumulated.last().map(|warning| warning.code), Some(0));
        assert!(
            accumulated
                .last()
                .is_some_and(|warning| warning.message.contains("仅保留前"))
        );
    }

    #[test]
    fn query_row_budget_enforces_bytes() {
        let row = || Row {
            values: vec![Value::Text("x".repeat(128))],
        };
        let one_row_bytes = row().retained_bytes();
        let mut rows = Vec::new();
        let mut retained_bytes = 0;

        assert_eq!(
            try_push_query_row(&mut rows, &mut retained_bytes, row(), one_row_bytes),
            Ok(())
        );
        assert_eq!(
            try_push_query_row(&mut rows, &mut retained_bytes, row(), one_row_bytes),
            Err(QueryResultLimit::Bytes)
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(retained_bytes, one_row_bytes);
    }

    #[test]
    fn query_result_memory_has_distinct_warning_and_truncation_thresholds() {
        assert!(
            query_result_memory_warning(QUERY_RESULT_MEMORY_WARNING_BYTES - 1, false, 10).is_none()
        );

        let warning = query_result_memory_warning(QUERY_RESULT_MEMORY_WARNING_BYTES, false, 20);
        assert!(
            warning
                .as_ref()
                .is_some_and(|warning| warning.message.contains("128 MiB"))
        );
        assert!(
            warning
                .as_ref()
                .is_some_and(|warning| warning.message.contains("未截断"))
        );
        assert!(
            warning
                .as_ref()
                .is_some_and(|warning| warning.message.contains("256 MiB"))
        );

        let truncated = query_result_memory_warning(MAX_QUERY_RESULT_BYTES, true, 30);
        assert!(
            truncated
                .as_ref()
                .is_some_and(|warning| warning.message.contains("256 MiB"))
        );
        assert!(
            truncated
                .as_ref()
                .is_some_and(|warning| warning.message.contains("已截断"))
        );
        assert!(
            truncated
                .as_ref()
                .is_some_and(|warning| !warning.message.contains("未截断"))
        );
    }

    #[test]
    fn query_result_column_metadata_is_bounded_and_consistent() {
        let columns = vec!["a".to_string(), "bb".to_string()];
        let types = vec!["x".to_string(), "yy".to_string()];

        assert!(matches!(
            validate_query_columns_with_limits(&columns, &types, 2, 6),
            Ok(6)
        ));
        assert!(validate_query_columns_with_limits(&columns, &types, 1, 6).is_err());
        assert!(validate_query_columns_with_limits(&columns, &types, 2, 5).is_err());
        assert!(validate_query_columns_with_limits(&columns, &types[..1], 2, 6).is_err());
    }

    #[test]
    fn backend_validation_runs_before_pool_cache_lookup() {
        let config = ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root");
        assert!(validate_backend_config(&config, DriverKind::Mysql, "mysql").is_ok());
        assert!(validate_backend_config(&config, DriverKind::Postgres, "postgres").is_err());

        let mut invalid = config;
        invalid.port = 0;
        assert!(validate_backend_config(&invalid, DriverKind::Mysql, "mysql").is_err());
    }

    #[test]
    fn metadata_identifiers_are_validated_before_pool_lookup() {
        assert!(validate_metadata_identifier("public", "schema").is_ok());
        assert!(validate_metadata_identifier("", "schema").is_err());
        assert!(validate_metadata_identifier("bad\nname", "table").is_err());
        assert!(
            validate_metadata_identifier(
                &"x".repeat(ramag_domain::entities::MAX_CONNECTION_IDENTIFIER_BYTES + 1),
                "table",
            )
            .is_err()
        );
    }
}
