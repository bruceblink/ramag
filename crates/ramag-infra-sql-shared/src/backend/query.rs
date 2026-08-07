use super::*;

pub(super) async fn run_select<B>(
    b: &B,
    conn: &mut <B::Db as Database>::Connection,
    sql: &str,
    max_result_bytes: u64,
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
            if let Err(limit) =
                try_push_query_row(&mut domain_rows, &mut retained_bytes, row, max_result_bytes)
            {
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

    let truncated = limit_reached.is_some();
    let warnings = query_result_memory_warning(
        retained_bytes,
        truncated,
        domain_rows.len(),
        max_result_bytes,
    )
    .into_iter()
    .collect();

    Ok(QueryResult {
        columns,
        column_types,
        rows: domain_rows,
        affected_rows: 0,
        elapsed_ms: 0,
        warnings,
        truncated,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryResultLimit {
    Bytes,
}

pub(super) fn query_result_memory_warning(
    retained_bytes: u64,
    truncated: bool,
    retained_rows: usize,
    max_result_bytes: u64,
) -> Option<Warning> {
    let warning_mib = QUERY_RESULT_MEMORY_WARNING_BYTES / (1024 * 1024);
    let maximum_mib = max_result_bytes / (1024 * 1024);
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

pub(super) fn validate_query_columns(columns: &[String], column_types: &[String]) -> Result<u64> {
    validate_query_columns_with_limits(
        columns,
        column_types,
        MAX_QUERY_RESULT_COLUMNS,
        MAX_QUERY_RESULT_METADATA_BYTES,
    )
}

pub(super) fn validate_query_columns_with_limits(
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

pub(super) fn try_push_query_row(
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

pub(super) async fn run_dml<B>(
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
        truncated: false,
    })
}
