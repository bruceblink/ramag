//! SQL 同步查询、批量写入与 PostgreSQL 枚举准备。

use super::*;

pub(super) async fn existing_identity_keys(
    service: &DataSyncService,
    target: &ramag_domain::entities::ConnectionConfig,
    driver: DriverKind,
    target_table: &str,
    identity_columns: &[String],
    text_columns: &HashSet<String>,
    identities: &[Row],
) -> Result<HashSet<String>> {
    let mut found = HashSet::new();
    for range in identity_ranges(driver, identities)? {
        let predicate = identity_predicate(driver, identity_columns, &identities[range])?;
        let query = format!(
            "SELECT {} FROM {target_table} WHERE {predicate};",
            source_select_columns(driver, identity_columns, text_columns)
        );
        let result = service
            .connection_service()
            .execute(
                target,
                &Query::new(query).with_result_byte_limit(TRANSFER_BATCH_BYTES),
            )
            .await?;
        if result.truncated {
            return Err(DomainError::InvalidConfig(
                "目标身份键查询超过安全字节上限，请缩小同步范围".into(),
            ));
        }
        for row in result.rows {
            found.insert(identity_key(&row.values)?);
        }
    }
    Ok(found)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn fetch_source_rows(
    service: &DataSyncService,
    source: &ramag_domain::entities::ConnectionConfig,
    driver: DriverKind,
    source_table: &str,
    writable_columns: &[String],
    identity_columns: &[String],
    identities: &[Vec<Value>],
    object: &SqlPreparedObject,
) -> Result<Vec<Row>> {
    let identity_rows: Vec<Row> = identities
        .iter()
        .cloned()
        .map(|values| Row { values })
        .collect();
    let mut output = Vec::with_capacity(identities.len());
    let mut pending: Vec<(usize, usize)> = identity_ranges(driver, &identity_rows)?
        .into_iter()
        .map(|range| (range.start, range.end))
        .collect();
    while let Some((start, end)) = pending.pop() {
        let predicate = identity_predicate(driver, identity_columns, &identity_rows[start..end])?;
        let query = format!(
            "SELECT {} FROM {source_table} WHERE {predicate} ORDER BY {};",
            source_select_columns(driver, writable_columns, &object.source_text_columns),
            quoted_columns(driver, identity_columns)
        );
        let result = service
            .connection_service()
            .execute(
                source,
                &Query::new(query).with_result_byte_limit(TRANSFER_BATCH_BYTES),
            )
            .await?;
        if result.truncated {
            if end - start <= 1 {
                return Err(oversized_row_error(object));
            }
            let middle = start + (end - start) / 2;
            pending.push((middle, end));
            pending.push((start, middle));
        } else {
            output.extend(result.rows);
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_rows(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    rows: &[Row],
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let driver = prepared.request.engine;
    let target = qualified(driver, &plan.scope.target_namespace, &object.mapping.target);
    let prefix = format!(
        "INSERT INTO {target} ({}){} VALUES ",
        quoted_columns(driver, &object.writable_columns),
        if driver == DriverKind::Postgres && object.has_identity_always {
            " OVERRIDING SYSTEM VALUE"
        } else {
            ""
        }
    );
    let conflict = match driver {
        DriverKind::Postgres => format!(
            " ON CONFLICT ({}) DO NOTHING",
            quoted_columns(driver, &object.identity.columns)
        ),
        DriverKind::Mysql => {
            let identity = driver.quote_identifier(&object.identity.columns[0]);
            format!(" ON DUPLICATE KEY UPDATE {identity} = {identity}")
        }
        DriverKind::Redis | DriverKind::Mongodb => unreachable!(),
    };
    let mut statement = prefix.clone();
    let mut buffered = 0usize;
    let mut buffered_rows = Vec::new();
    for row in rows {
        let tuple = row_tuple(row, driver);
        let prospective = statement
            .len()
            .saturating_add(usize::from(buffered > 0) * 2)
            .saturating_add(tuple.len())
            .saturating_add(conflict.len())
            .saturating_add(1);
        if prospective > TRANSFER_BATCH_BYTES && buffered == 0 {
            return Err(oversized_row_error(object));
        }
        if buffered > 0 && (buffered >= TRANSFER_BATCH_ITEMS || prospective > TRANSFER_BATCH_BYTES)
        {
            let inserted = flush_insert(
                service,
                prepared,
                plan,
                object,
                &mut statement,
                &conflict,
                &buffered_rows,
                permit,
                progress,
                summary,
            )
            .await?;
            let _ = inserted;
            statement.push_str(&prefix);
            buffered = 0;
            buffered_rows.clear();
        }
        if buffered > 0 {
            statement.push_str(", ");
        }
        statement.push_str(&tuple);
        buffered += 1;
        buffered_rows.push(row.clone());
    }
    if buffered > 0 {
        flush_insert(
            service,
            prepared,
            plan,
            object,
            &mut statement,
            &conflict,
            &buffered_rows,
            permit,
            progress,
            summary,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn flush_insert(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    statement: &mut String,
    conflict: &str,
    rows: &[Row],
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<u64> {
    let attempted = rows.len();
    statement.push_str(conflict);
    statement.push(';');
    progress.stage = DataSyncStage::Writing;
    service.gate().update_progress(permit, progress.clone());
    let result = service
        .connection_service()
        .execute(&prepared.target, &Query::new(std::mem::take(statement)))
        .await?;
    let failed = if prepared.request.engine == DriverKind::Mysql {
        mysql_missing_identities_after_insert(service, prepared, plan, object, rows).await?
    } else {
        0
    };
    let inserted = result
        .affected_rows
        .min((attempted as u64).saturating_sub(failed));
    let skipped = (attempted as u64)
        .saturating_sub(inserted)
        .saturating_sub(failed);
    progress.add_inserted(inserted);
    progress.add_skipped(skipped);
    progress.add_failed(failed);
    summary.inserted = summary.inserted.saturating_add(inserted);
    summary.skipped = summary.skipped.saturating_add(skipped);
    summary.failed = summary.failed.saturating_add(failed);
    service.gate().update_progress(permit, progress.clone());
    if failed > 0 {
        return Err(DomainError::QueryFailed(format!(
            "MySQL 表 {} 有 {failed} 行与目标的非身份唯一约束冲突；已停止同步，目标已有数据未被覆盖",
            object.mapping.target
        )));
    }
    Ok(inserted)
}

pub(super) async fn mysql_missing_identities_after_insert(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    rows: &[Row],
) -> Result<u64> {
    let positions = identity_positions(object)?;
    let identities: Vec<Row> = rows
        .iter()
        .map(|row| {
            positions
                .iter()
                .map(|position| {
                    row.values.get(*position).cloned().ok_or_else(|| {
                        DomainError::QueryFailed("MySQL 写后身份键结果列缺失".into())
                    })
                })
                .collect::<Result<Vec<_>>>()
                .map(|values| Row { values })
        })
        .collect::<Result<_>>()?;
    let target_table = qualified(
        DriverKind::Mysql,
        &plan.scope.target_namespace,
        &object.mapping.target,
    );
    let existing = existing_identity_keys(
        service,
        &prepared.target,
        DriverKind::Mysql,
        &target_table,
        &object.identity.columns,
        &object.source_text_columns,
        &identities,
    )
    .await?;
    Ok((identities.len().saturating_sub(existing.len())) as u64)
}

pub(super) async fn execute_statement(
    service: &DataSyncService,
    target: &ramag_domain::entities::ConnectionConfig,
    statement: &str,
) -> Result<()> {
    if statement.len() > TRANSFER_BATCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "结构 SQL 超过 {} MiB 安全上限",
            TRANSFER_BATCH_BYTES / 1024 / 1024
        )));
    }
    service
        .connection_service()
        .execute(target, &Query::new(statement.to_string()))
        .await?;
    Ok(())
}

pub(super) async fn ensure_postgres_enums(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
) -> Result<()> {
    if prepared.target.driver != DriverKind::Postgres || plan.postgres_enums.is_empty() {
        return Ok(());
    }
    let mut target_enums =
        load_postgres_enum_definitions(service, &prepared.target, &plan.scope.target_namespace)
            .await?;
    for expected in &plan.postgres_enums {
        if let Some(existing) = target_enums.get(&expected.name) {
            if existing == &expected.signature {
                continue;
            }
            return Err(incompatible_postgres_enum_error(
                &plan.scope.target_namespace,
                &expected.name,
            ));
        }
        if let Err(create_error) =
            execute_statement(service, &prepared.target, &expected.create_statement).await
        {
            // 快照检查与 CREATE TYPE 之间仍可能有并发创建；重新读取后只复用完全相同的定义。
            target_enums = load_postgres_enum_definitions(
                service,
                &prepared.target,
                &plan.scope.target_namespace,
            )
            .await?;
            match target_enums.get(&expected.name) {
                Some(existing) if existing == &expected.signature => continue,
                Some(_) => {
                    return Err(incompatible_postgres_enum_error(
                        &plan.scope.target_namespace,
                        &expected.name,
                    ));
                }
                None => return Err(create_error),
            }
        }
        target_enums.insert(expected.name.clone(), expected.signature.clone());
    }
    Ok(())
}
