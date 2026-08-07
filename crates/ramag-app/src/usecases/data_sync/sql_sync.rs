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
mod identity;
mod io;

use super::sql_preflight::current_sql_target_snapshot;
use crate::usecases::transfer::sql_catalog::transfer_literal;
use identity::*;
use io::*;

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
