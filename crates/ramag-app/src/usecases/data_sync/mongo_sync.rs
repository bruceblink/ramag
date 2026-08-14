//! MongoDB 数据同步执行。

use ramag_domain::entities::{DataSyncProgress, DataSyncStage, DataSyncSummary};
use ramag_domain::error::{DomainError, Result};
use serde_json::{Value, json};

use super::gate::DataSyncPermit;
use super::mongo_preflight::current_mongo_target_snapshot;
use super::service::{DataSyncService, MongoPreparedObject, MongoPreparedPlan, PreparedDataSync};

mod support;

pub(super) async fn run_mongo_sync(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &MongoPreparedPlan,
    permit: &DataSyncPermit,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let current = current_mongo_target_snapshot(service, &prepared.target, plan)
        .await
        .map_err(|error| {
            tracing::error!(
                operation = "data_sync_mongo",
                task_id = %permit.task_id(),
                stage = "verify_target",
                target_database = %plan.scope.target_database,
                error = %error,
                "load MongoDB target snapshot failed"
            );
            error
        })?;
    if current != plan.target_snapshot {
        let error =
            DomainError::InvalidConfig("目标 MongoDB 结构已在预检后变化，请重新预检并确认".into());
        tracing::warn!(
            operation = "data_sync_mongo",
            task_id = %permit.task_id(),
            stage = "verify_target",
            source_database = %plan.scope.source_database,
            target_database = %plan.scope.target_database,
            error = %error,
            "MongoDB target changed after preflight"
        );
        return Err(error);
    }
    let mut progress = DataSyncProgress {
        stage: DataSyncStage::VerifyingTarget,
        objects_total: Some(plan.objects.len() as u64),
        ..DataSyncProgress::default()
    };
    service.gate().update_progress(permit, progress.clone());

    for object in &plan.objects {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            break;
        }
        progress.object = format!("{} → {}", object.mapping.source, object.mapping.target);
        if let Err(error) = sync_collection(
            service,
            prepared,
            plan,
            object,
            permit,
            &mut progress,
            summary,
        )
        .await
        {
            tracing::error!(
                operation = "data_sync_mongo",
                task_id = %permit.task_id(),
                stage = ?progress.stage,
                source_database = %plan.scope.source_database,
                target_database = %plan.scope.target_database,
                source_collection = %object.mapping.source,
                target_collection = %object.mapping.target,
                error = %error,
                "MongoDB collection sync failed"
            );
            return Err(error);
        }
        if summary.cancelled {
            break;
        }
        summary.objects = summary.objects.saturating_add(1);
        progress.objects_done = progress.objects_done.saturating_add(1);
        service.gate().update_progress(permit, progress.clone());
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
async fn sync_collection(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &MongoPreparedPlan,
    object: &MongoPreparedObject,
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    if !object.target_exists {
        progress.stage = DataSyncStage::CreatingStructure;
        service.gate().update_progress(permit, progress.clone());
        support::create_collection(
            service,
            prepared,
            &plan.scope.target_database,
            &object.mapping.target,
            &object.source_blueprint,
        )
        .await?;
        sync_new_collection(service, prepared, plan, object, permit, progress, summary).await?;
    } else {
        sync_existing_collection(service, prepared, plan, object, permit, progress, summary)
            .await?;
    }
    if summary.cancelled {
        return Ok(());
    }
    if !object.missing_indexes.is_empty() {
        progress.stage = DataSyncStage::Finalizing;
        service.gate().update_progress(permit, progress.clone());
        support::create_indexes(
            service,
            prepared,
            &plan.scope.target_database,
            &object.mapping.target,
            &object.missing_indexes,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn sync_new_collection(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &MongoPreparedPlan,
    object: &MongoPreparedObject,
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let mut last_id = None;
    loop {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Scanning;
        service.gate().update_progress(permit, progress.clone());
        let result = service
            .mongo_service()
            .find(
                &prepared.source,
                &plan.scope.source_database,
                &object.mapping.source,
                &support::page_spec(last_id.as_ref(), None),
            )
            .await?;
        if result.documents.is_empty() {
            return Ok(());
        }
        let next_id = support::document_id(
            result
                .documents
                .last()
                .ok_or_else(|| DomainError::QueryFailed("MongoDB 分页结果为空".into()))?,
        )?
        .clone();
        progress.add_scanned(result.documents.len() as u64);
        summary.scanned = summary
            .scanned
            .saturating_add(result.documents.len() as u64);
        support::insert_documents(
            service,
            prepared,
            &plan.scope.target_database,
            &object.mapping.target,
            result.documents,
            permit,
            progress,
            summary,
        )
        .await?;
        last_id = Some(next_id);
    }
}

#[allow(clippy::too_many_arguments)]
async fn sync_existing_collection(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &MongoPreparedPlan,
    object: &MongoPreparedObject,
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let mut last_id = None;
    loop {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        progress.stage = DataSyncStage::Scanning;
        service.gate().update_progress(permit, progress.clone());
        let result = service
            .mongo_service()
            .find(
                &prepared.source,
                &plan.scope.source_database,
                &object.mapping.source,
                &support::page_spec(last_id.as_ref(), Some(json!({"_id": 1}))),
            )
            .await?;
        if result.documents.is_empty() {
            return Ok(());
        }
        let ids: Vec<Value> = result
            .documents
            .iter()
            .map(support::document_id)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .cloned()
            .collect();
        last_id = ids.last().cloned();
        progress.add_scanned(ids.len() as u64);
        summary.scanned = summary.scanned.saturating_add(ids.len() as u64);

        let existing = support::existing_id_keys(
            service,
            &prepared.target,
            &plan.scope.target_database,
            &object.mapping.target,
            &ids,
        )
        .await?;
        let mut missing = Vec::new();
        for id in ids {
            if existing.contains(&support::id_key(&id)?) {
                progress.add_skipped(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                missing.push(id);
            }
        }
        let documents = support::fetch_documents_by_ids(
            service,
            &prepared.source,
            &plan.scope.source_database,
            &object.mapping.source,
            missing,
            permit,
        )
        .await?;
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        let missing_after_scan =
            support::ids_missing_after_fetch(&existing, &documents, result.documents.len())?;
        if missing_after_scan > 0 {
            progress.add_skipped(missing_after_scan);
            summary.skipped = summary.skipped.saturating_add(missing_after_scan);
            summary.push_warning(format!(
                "源 Collection {} 有 {missing_after_scan} 个文档在读取期间消失",
                object.mapping.source
            ));
        }
        support::insert_documents(
            service,
            prepared,
            &plan.scope.target_database,
            &object.mapping.target,
            documents,
            permit,
            progress,
            summary,
        )
        .await?;
        service.gate().update_progress(permit, progress.clone());
    }
}
