//! SQL 数据同步执行：身份键 keyset、批量判重、缺失行回源、批量插入。

use std::collections::HashSet;

use ramag_domain::entities::{
    DataSyncProgress, DataSyncStage, DataSyncSummary, DriverKind, Query, QueryResult, Row,
    TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, Value,
};
use ramag_domain::error::{DomainError, Result};

use super::gate::DataSyncPermit;
use super::postgres_enum::{incompatible_postgres_enum_error, load_postgres_enum_definitions};
use super::service::{DataSyncService, PreparedDataSync, SqlPreparedObject, SqlPreparedPlan};
use super::sql_ddl::qualified;
use super::sql_preflight::current_sql_target_snapshot;
use crate::usecases::transfer::sql_catalog::transfer_literal;

const ID_QUERY_BYTES: usize = 8 * 1024 * 1024;
const ID_QUERY_ITEMS: usize = 1_000;

pub(super) async fn run_sql_sync(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    permit: &DataSyncPermit,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let mappings: Vec<_> = plan
        .objects
        .iter()
        .map(|object| object.mapping.clone())
        .collect();
    let postgres_enum_names: Vec<_> = plan
        .postgres_enums
        .iter()
        .map(|item| item.name.clone())
        .collect();
    let current = current_sql_target_snapshot(
        service,
        &prepared.target,
        &plan.scope,
        &mappings,
        &postgres_enum_names,
    )
    .await?;
    if current != plan.target_snapshot {
        return Err(DomainError::InvalidConfig(
            "目标 SQL 结构已在预检后变化，请重新预检并确认".into(),
        ));
    }
    let mut progress = DataSyncProgress {
        stage: DataSyncStage::VerifyingTarget,
        objects_total: Some(plan.objects.len() as u64),
        ..DataSyncProgress::default()
    };
    service.gate().update_progress(permit, progress.clone());
    if permit.cancellation_requested() {
        summary.cancelled = true;
        return Ok(());
    }

    let creates_structure =
        !plan.namespace_exists || plan.objects.iter().any(|object| !object.target_exists);
    if let Some(statement) = &plan.namespace_create {
        progress.stage = DataSyncStage::CreatingStructure;
        service.gate().update_progress(permit, progress.clone());
        execute_statement(service, &prepared.target, statement).await?;
    }
    ensure_postgres_enums(service, prepared, plan).await?;
    for statement in &plan.pre_create_statements {
        execute_statement(service, &prepared.target, statement).await?;
    }
    for object in &plan.objects {
        if let Some(statement) = &object.create_statement {
            progress.stage = DataSyncStage::CreatingStructure;
            progress.object = object_label(object);
            service.gate().update_progress(permit, progress.clone());
            execute_statement(service, &prepared.target, statement).await?;
            for statement in &object.post_create_statements {
                execute_statement(service, &prepared.target, statement).await?;
            }
        }
    }

    let mut cancellation_deferred = false;
    for object in &plan.objects {
        if permit.cancellation_requested() && !creates_structure {
            summary.cancelled = true;
            break;
        }
        if permit.cancellation_requested() && creates_structure && !cancellation_deferred {
            cancellation_deferred = true;
            summary.push_warning(
                "已创建目标结构；为避免留下不完整表或约束，取消请求将在结构安全收尾后生效",
            );
        }
        progress.object = object_label(object);
        sync_table(
            service,
            prepared,
            plan,
            object,
            permit,
            !creates_structure,
            &mut progress,
            summary,
        )
        .await?;
        if summary.cancelled {
            break;
        }
        summary.objects = summary.objects.saturating_add(1);
        progress.objects_done = progress.objects_done.saturating_add(1);
        service.gate().update_progress(permit, progress.clone());
    }

    if !summary.cancelled {
        progress.stage = DataSyncStage::Finalizing;
        service.gate().update_progress(permit, progress.clone());
        for object in &plan.objects {
            for statement in &object.final_statements {
                execute_statement(service, &prepared.target, statement).await?;
            }
        }
    }
    if cancellation_deferred {
        summary.cancelled = true;
    }
    progress.stage = if summary.cancelled {
        DataSyncStage::Cancelling
    } else {
        DataSyncStage::Finalizing
    };
    service.gate().update_progress(permit, progress);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn sync_table(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    permit: &DataSyncPermit,
    honor_cancel: bool,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    if object.target_exists {
        sync_existing_table(
            service,
            prepared,
            plan,
            object,
            permit,
            honor_cancel,
            progress,
            summary,
        )
        .await
    } else {
        sync_new_table(
            service,
            prepared,
            plan,
            object,
            permit,
            honor_cancel,
            progress,
            summary,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn sync_new_table(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    permit: &DataSyncPermit,
    honor_cancel: bool,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let driver = prepared.request.engine;
    let source_table = qualified(driver, &plan.scope.source_namespace, &object.mapping.source);
    let columns = source_select_columns(
        driver,
        &object.writable_columns,
        &object.source_text_columns,
    );
    let identity_positions = identity_positions(object)?;
    let mut last_key = None;
    loop {
        if honor_cancel && permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Scanning;
        service.gate().update_progress(permit, progress.clone());
        let query = keyset_select(
            driver,
            &source_table,
            &columns,
            &object.identity.columns,
            last_key.as_deref(),
        );
        let result = service
            .connection_service()
            .execute(
                &prepared.source,
                &Query::new(query).with_result_byte_limit(TRANSFER_BATCH_BYTES),
            )
            .await?;
        if result.rows.is_empty() {
            if result.truncated {
                return Err(oversized_row_error(object));
            }
            return Ok(());
        }
        last_key = Some(last_identity(&result, &identity_positions, object)?);
        add_scanned(
            result.rows.len() as u64,
            result.retained_bytes(),
            progress,
            summary,
        );
        insert_rows(
            service,
            prepared,
            plan,
            object,
            &result.rows,
            permit,
            progress,
            summary,
        )
        .await?;
    }
}

#[allow(clippy::too_many_arguments)]
async fn sync_existing_table(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &SqlPreparedPlan,
    object: &SqlPreparedObject,
    permit: &DataSyncPermit,
    honor_cancel: bool,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let driver = prepared.request.engine;
    let source_table = qualified(driver, &plan.scope.source_namespace, &object.mapping.source);
    let target_table = qualified(driver, &plan.scope.target_namespace, &object.mapping.target);
    let identity_columns = source_select_columns(
        driver,
        &object.identity.columns,
        &object.source_text_columns,
    );
    let mut last_key = None;
    loop {
        if honor_cancel && permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Scanning;
        service.gate().update_progress(permit, progress.clone());
        let query = keyset_select(
            driver,
            &source_table,
            &identity_columns,
            &object.identity.columns,
            last_key.as_deref(),
        );
        let ids = service
            .connection_service()
            .execute(
                &prepared.source,
                &Query::new(query).with_result_byte_limit(TRANSFER_BATCH_BYTES),
            )
            .await?;
        if ids.rows.is_empty() {
            if ids.truncated {
                return Err(oversized_row_error(object));
            }
            return Ok(());
        }
        let identity_positions: Vec<usize> = (0..object.identity.columns.len()).collect();
        last_key = Some(last_identity(&ids, &identity_positions, object)?);
        add_scanned(
            ids.rows.len() as u64,
            ids.retained_bytes(),
            progress,
            summary,
        );

        let existing = existing_identity_keys(
            service,
            &prepared.target,
            driver,
            &target_table,
            &object.identity.columns,
            &object.source_text_columns,
            &ids.rows,
        )
        .await?;
        let mut missing = Vec::new();
        for row in &ids.rows {
            let key = identity_key(&row.values)?;
            if existing.contains(&key) {
                progress.add_skipped(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                missing.push(row.values.clone());
            }
        }
        if missing.is_empty() {
            service.gate().update_progress(permit, progress.clone());
            continue;
        }
        if honor_cancel && permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        let rows = fetch_source_rows(
            service,
            &prepared.source,
            driver,
            &source_table,
            &object.writable_columns,
            &object.identity.columns,
            &missing,
            object,
        )
        .await?;
        if rows.len() < missing.len() {
            summary.push_warning(format!(
                "源表 {} 在扫描后有 {} 条记录消失，已安全跳过",
                object.mapping.source,
                missing.len() - rows.len()
            ));
        }
        insert_rows(
            service, prepared, plan, object, &rows, permit, progress, summary,
        )
        .await?;
    }
}

async fn existing_identity_keys(
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
async fn fetch_source_rows(
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
async fn insert_rows(
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
async fn flush_insert(
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

async fn mysql_missing_identities_after_insert(
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

async fn execute_statement(
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

async fn ensure_postgres_enums(
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

fn keyset_select(
    driver: DriverKind,
    table: &str,
    selected_columns: &str,
    identity_columns: &[String],
    last_key: Option<&[Value]>,
) -> String {
    let order = quoted_columns(driver, identity_columns);
    let predicate = last_key.map_or(String::new(), |values| {
        let literals = values
            .iter()
            .map(|value| transfer_literal(value, driver))
            .collect::<Vec<_>>()
            .join(", ");
        if identity_columns.len() == 1 {
            format!(
                " WHERE {} > {literals}",
                driver.quote_identifier(&identity_columns[0])
            )
        } else {
            format!(" WHERE ({order}) > ({literals})")
        }
    });
    format!(
        "SELECT {selected_columns} FROM {table}{predicate} ORDER BY {order} LIMIT {TRANSFER_BATCH_ITEMS};"
    )
}

fn identity_predicate(driver: DriverKind, columns: &[String], rows: &[Row]) -> Result<String> {
    if rows.is_empty() {
        return Err(DomainError::Other("身份键批次不能为空".into()));
    }
    for row in rows {
        if row.values.len() != columns.len() {
            return Err(DomainError::QueryFailed("身份键列数与结果不一致".into()));
        }
    }
    if columns.len() == 1 {
        let values = rows
            .iter()
            .map(|row| transfer_literal(&row.values[0], driver))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "{} IN ({values})",
            driver.quote_identifier(&columns[0])
        ))
    } else {
        let values = rows
            .iter()
            .map(|row| {
                format!(
                    "({})",
                    row.values
                        .iter()
                        .map(|value| transfer_literal(value, driver))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "({}) IN ({values})",
            quoted_columns(driver, columns)
        ))
    }
}

fn identity_ranges(driver: DriverKind, identities: &[Row]) -> Result<Vec<std::ops::Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    for (index, row) in identities.iter().enumerate() {
        let row_bytes = row
            .values
            .iter()
            .map(|value| transfer_literal(value, driver).len().saturating_add(2))
            .sum::<usize>();
        if row_bytes > TRANSFER_BATCH_BYTES {
            return Err(DomainError::InvalidConfig(
                "单个 SQL 身份键超过安全字节上限".into(),
            ));
        }
        if index > start
            && (index - start >= ID_QUERY_ITEMS || bytes.saturating_add(row_bytes) > ID_QUERY_BYTES)
        {
            ranges.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(row_bytes);
    }
    if start < identities.len() {
        ranges.push(start..identities.len());
    }
    Ok(ranges)
}

fn identity_positions(object: &SqlPreparedObject) -> Result<Vec<usize>> {
    object
        .identity
        .columns
        .iter()
        .map(|name| {
            object
                .writable_columns
                .iter()
                .position(|column| column == name)
                .ok_or_else(|| DomainError::InvalidConfig(format!("记录身份列 {name} 不可写入")))
        })
        .collect()
}

fn last_identity(
    result: &QueryResult,
    positions: &[usize],
    object: &SqlPreparedObject,
) -> Result<Vec<Value>> {
    let row = result
        .rows
        .last()
        .ok_or_else(|| DomainError::QueryFailed("SQL 分页结果为空".into()))?;
    positions
        .iter()
        .map(|position| {
            row.values.get(*position).cloned().ok_or_else(|| {
                DomainError::QueryFailed(format!("表 {} 的身份键结果列缺失", object.mapping.source))
            })
        })
        .collect()
}

fn identity_key(values: &[Value]) -> Result<String> {
    serde_json::to_string(values)
        .map_err(|error| DomainError::Other(format!("序列化 SQL 身份键失败：{error}")))
}

fn quoted_columns(driver: DriverKind, columns: &[String]) -> String {
    columns
        .iter()
        .map(|column| driver.quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}

fn source_select_columns(
    driver: DriverKind,
    columns: &[String],
    text_columns: &HashSet<String>,
) -> String {
    columns
        .iter()
        .map(|column| {
            let quoted = driver.quote_identifier(column);
            if driver == DriverKind::Postgres && text_columns.contains(column) {
                format!("CAST({quoted} AS TEXT) AS {quoted}")
            } else {
                quoted
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_tuple(row: &Row, driver: DriverKind) -> String {
    format!(
        "({})",
        row.values
            .iter()
            .map(|value| transfer_literal(value, driver))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn add_scanned(
    rows: u64,
    bytes: u64,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) {
    progress.add_scanned(rows);
    progress.add_bytes(bytes);
    summary.scanned = summary.scanned.saturating_add(rows);
    summary.bytes = summary.bytes.saturating_add(bytes);
}

fn object_label(object: &SqlPreparedObject) -> String {
    format!("{} → {}", object.mapping.source, object.mapping.target)
}

fn oversized_row_error(object: &SqlPreparedObject) -> DomainError {
    DomainError::InvalidConfig(format!(
        "表 {} 的单行或身份键超过 {} MiB 安全上限",
        object.mapping.source,
        TRANSFER_BATCH_BYTES / 1024 / 1024
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyset_for_composite_identity_uses_row_constructor() {
        let sql = keyset_select(
            DriverKind::Postgres,
            "\"public\".\"items\"",
            "\"id\", \"part\"",
            &["id".into(), "part".into()],
            Some(&[Value::Int(2), Value::Text("x".into())]),
        );
        assert!(sql.contains("WHERE (\"id\", \"part\") > (2, 'x')"));
        assert!(!sql.contains("OFFSET"));
    }

    #[test]
    fn identity_predicate_supports_composite_keys() {
        let rows = [
            Row {
                values: vec![Value::Int(1), Value::Text("a".into())],
            },
            Row {
                values: vec![Value::Int(2), Value::Text("b".into())],
            },
        ];
        let predicate =
            identity_predicate(DriverKind::Mysql, &["id".into(), "part".into()], &rows).unwrap();
        assert_eq!(predicate, "(`id`, `part`) IN ((1, 'a'), (2, 'b'))");
    }

    #[test]
    fn identity_ranges_are_bounded_by_item_count() {
        let rows: Vec<Row> = (0..2_001)
            .map(|value| Row {
                values: vec![Value::Int(value)],
            })
            .collect();
        let ranges = identity_ranges(DriverKind::Postgres, &rows).unwrap();
        assert_eq!(ranges, [0..1_000, 1_000..2_000, 2_000..2_001]);
    }

    #[test]
    fn postgres_custom_types_are_selected_as_text_without_changing_order_columns() {
        let columns = vec!["id".into(), "states".into()];
        let text_columns = HashSet::from(["states".into()]);
        assert_eq!(
            source_select_columns(DriverKind::Postgres, &columns, &text_columns),
            "\"id\", CAST(\"states\" AS TEXT) AS \"states\""
        );
        assert_eq!(
            source_select_columns(DriverKind::Mysql, &columns, &text_columns),
            "`id`, `states`"
        );
    }
}
