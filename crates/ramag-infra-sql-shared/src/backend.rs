//! SqlBackend：SQL 类 driver 唯一抽象层 + 泛型模板（test/execute/cancel/list_*）。
//! driver crate 仅实现方言方法 + 行解码 + 元数据 SQL，由 `impl_driver_for!` 宏代理到 Driver

use std::time::Instant;

use async_trait::async_trait;
use ramag_domain::entities::{
    Column, ConnectionConfig, ForeignKey, Index, Query, QueryResult, Row, Schema, Table, Value,
    Warning,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use ramag_domain::traits::CancelHandle;
use sqlx::pool::PoolConnection;
use sqlx::{Database, Executor, IntoArguments, Pool};
use tracing::{debug, info, warn};

use crate::errors::map_sqlx_common;
use crate::pool::PoolCache;
use crate::sql::{
    SplitOptions, first_keyword, inject_limit_if_needed, is_query_returning_rows,
    is_write_statement, split_statements, sql_has_no_limit_marker,
};

/// 单次查询保留的警告上限，包含可能的截断提示。
pub const MAX_QUERY_WARNINGS: usize = 1_000;

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
    // 在等待建池锁前固定请求代际；若期间配置被 evict，本次旧请求可完成，
    // 但不得命中新配置的池，也不得把旧池重新写回缓存。
    let generation = b.cache().generation(&config.id);
    if let Some(p) = b.cache().get(&config.id, generation) {
        return Ok(p);
    }
    let build_lock = b.cache().build_lock(&config.id, generation);
    let _guard = build_lock.lock().await;
    if b.cache().generation(&config.id) != generation {
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
    let pool = get_pool(b, config).await?;
    b.list_foreign_keys_impl(&pool, schema, table).await
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

    let statements = split_statements(&query.sql, b.split_options());
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

        let r = if is_select {
            run_select::<B>(b, &mut *conn, effective_sql).await?
        } else {
            run_dml::<B>(b, &mut *conn, effective_sql).await?
        };
        if !is_select {
            total_affected = total_affected.saturating_add(r.affected_rows);
        }
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
    let rows = sqlx::query(sql)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| map_err(b, e))?;

    let (columns, column_types) = if let Some(first) = rows.first() {
        b.extract_columns(first)
    } else {
        b.extract_columns_fallback(conn, sql)
            .await
            .unwrap_or_default()
    };

    let domain_rows: Vec<Row> = rows
        .iter()
        .map(|row| b.decode_row(row).map(|values| Row { values }))
        .collect::<Result<Vec<_>>>()?;

    Ok(QueryResult {
        columns,
        column_types,
        rows: domain_rows,
        affected_rows: 0,
        elapsed_ms: 0,
        warnings: Vec::new(),
    })
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
    use super::{MAX_QUERY_WARNINGS, append_warnings_bounded};
    use ramag_domain::entities::Warning;

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
}
