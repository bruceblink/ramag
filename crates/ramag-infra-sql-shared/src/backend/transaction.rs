use super::*;

/// Begins a transaction on one pool connection and keeps that connection leased until finish.
pub async fn begin_transaction_impl<B>(
    b: &B,
    config: &ConnectionConfig,
) -> Result<ramag_domain::entities::TransactionId>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let pool = get_pool(b, config).await?;
    let mut transaction = pool.begin().await.map_err(|error| map_err(b, error))?;
    if let Some(schema) = config
        .database
        .as_deref()
        .filter(|schema| !schema.is_empty())
        && let Some(use_sql) = b.use_database_sql(schema)
    {
        (&mut *transaction)
            .execute(use_sql.as_str())
            .await
            .map_err(|error| map_err(b, error))?;
    }
    let transaction_id = b
        .transaction_store()
        .insert(config.id.clone(), transaction)?;
    info!(
        operation = "sql_transaction_begin",
        connection_id = %config.id,
        transaction_id = %transaction_id,
        "transaction started"
    );
    Ok(transaction_id)
}

/// Executes one or more statements on a leased transaction without committing it.
pub async fn execute_in_transaction_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    transaction_id: &ramag_domain::entities::TransactionId,
    query: &Query,
) -> Result<QueryResult>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    query.validate()?;
    if query.transactional {
        return Err(DomainError::InvalidConfig(
            "事务会话内不能嵌套自动提交事务".into(),
        ));
    }
    let slot = b
        .transaction_store()
        .get(&config.id, transaction_id)
        .ok_or_else(|| DomainError::QueryFailed("事务不存在或不属于当前连接".into()))?;
    let mut guard = slot.lock().await;
    let transaction = guard
        .as_mut()
        .ok_or_else(|| DomainError::QueryFailed("事务已经结束".into()))?;
    let start = Instant::now();
    let statements = split_statements_bounded(&query.sql, b.split_options(), MAX_SQL_STATEMENTS)?;
    if statements.is_empty() {
        return Ok(empty_query_result(start));
    }
    if statements
        .iter()
        .any(|statement| is_transaction_control_statement(statement))
    {
        return Err(DomainError::InvalidConfig(
            "手动事务内请使用事务控制按钮，不要执行 BEGIN、COMMIT 或 ROLLBACK".into(),
        ));
    }
    if config.production
        && let Some(stmt) = statements.iter().find(|s| is_write_statement(s))
    {
        warn!(
            operation = "sql_transaction_write_guard",
            conn = %config.name,
            keyword = first_keyword(stmt).as_deref().unwrap_or("?"),
            "read-only write blocked"
        );
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if let Some(schema) = query
        .default_schema
        .as_deref()
        .filter(|schema| !schema.is_empty())
        && let Some(use_sql) = b.use_database_sql(schema)
    {
        (&mut **transaction)
            .execute(use_sql.as_str())
            .await
            .map_err(|error| map_err(b, error))?;
    }

    let last_idx = statements.len() - 1;
    let mut total_affected = 0;
    let mut accumulated_warnings = Vec::new();
    let mut last_result = empty_query_result(start);
    execute_statements(
        b,
        &mut **transaction,
        query,
        &statements,
        last_idx,
        sql_has_no_limit_marker(&query.sql),
        &mut total_affected,
        &mut accumulated_warnings,
        &mut last_result,
    )
    .await?;
    if last_result.rows.is_empty() && last_result.columns.is_empty() {
        last_result.affected_rows = total_affected;
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    info!(
        operation = "sql_transaction_query",
        connection_id = %config.id,
        transaction_id = %transaction_id,
        elapsed_ms,
        affected = last_result.affected_rows,
        statements = statements.len(),
        "transaction query completed"
    );
    Ok(QueryResult {
        elapsed_ms,
        warnings: accumulated_warnings,
        ..last_result
    })
}

/// Commits a transaction and releases its leased connection.
pub async fn commit_transaction_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    transaction_id: &ramag_domain::entities::TransactionId,
) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    finish_transaction(b, config, transaction_id, true).await
}

/// Rolls back a transaction and releases its leased connection.
pub async fn rollback_transaction_impl<B>(
    b: &B,
    config: &ConnectionConfig,
    transaction_id: &ramag_domain::entities::TransactionId,
) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    finish_transaction(b, config, transaction_id, false).await
}

async fn finish_transaction<B>(
    b: &B,
    config: &ConnectionConfig,
    transaction_id: &ramag_domain::entities::TransactionId,
    commit: bool,
) -> Result<()>
where
    B: SqlBackend,
    for<'q> <B::Db as Database>::Arguments<'q>: IntoArguments<'q, B::Db>,
    for<'c> &'c Pool<B::Db>: Executor<'c, Database = B::Db>,
    for<'c> &'c mut <B::Db as Database>::Connection: Executor<'c, Database = B::Db>,
{
    let slot = b
        .transaction_store()
        .remove(&config.id, transaction_id)
        .ok_or_else(|| DomainError::QueryFailed("事务不存在或已经结束".into()))?;
    let transaction = slot
        .lock()
        .await
        .take()
        .ok_or_else(|| DomainError::QueryFailed("事务已经结束".into()))?;
    let result = if commit {
        transaction.commit().await
    } else {
        transaction.rollback().await
    };
    result.map_err(|error| map_err(b, error))?;
    info!(
        operation = if commit {
            "sql_transaction_commit"
        } else {
            "sql_transaction_rollback"
        },
        connection_id = %config.id,
        transaction_id = %transaction_id,
        "transaction finished"
    );
    Ok(())
}

fn empty_query_result(start: Instant) -> QueryResult {
    QueryResult {
        columns: Vec::new(),
        column_types: Vec::new(),
        rows: Vec::new(),
        affected_rows: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
        warnings: Vec::new(),
        truncated: false,
    }
}
