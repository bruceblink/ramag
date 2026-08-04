//! MongoDB 连接同步：`_id` Keyset、批量判重、缺失文档回源与延后建索引。

use std::collections::{HashMap, HashSet};

use ramag_domain::entities::{
    DataSyncProgress, DataSyncStage, DataSyncSummary, MongoDocument, MongoQuerySpec,
    TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, mongo_value_retained_bytes,
};
use ramag_domain::error::{DomainError, Result};
use serde_json::{Map, Value, json};

use super::gate::DataSyncPermit;
use super::mongo_preflight::current_mongo_target_snapshot;
use super::service::{
    DataSyncService, MongoCollectionBlueprint, MongoPreparedObject, MongoPreparedPlan,
    PreparedDataSync,
};

const ID_FILTER_BYTES: usize = 8 * 1024 * 1024;

pub(super) async fn run_mongo_sync(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    plan: &MongoPreparedPlan,
    permit: &DataSyncPermit,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let current = current_mongo_target_snapshot(service, &prepared.target, plan).await?;
    if current != plan.target_snapshot {
        return Err(DomainError::InvalidConfig(
            "目标 MongoDB 结构已在预检后变化，请重新预检并确认".into(),
        ));
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
        if !object.target_exists {
            progress.stage = DataSyncStage::CreatingStructure;
            service.gate().update_progress(permit, progress.clone());
            create_collection(
                service,
                prepared,
                &plan.scope.target_database,
                &object.mapping.target,
                &object.source_blueprint,
            )
            .await?;
            sync_new_collection(
                service,
                prepared,
                plan,
                object,
                permit,
                &mut progress,
                summary,
            )
            .await?;
        } else {
            sync_existing_collection(
                service,
                prepared,
                plan,
                object,
                permit,
                &mut progress,
                summary,
            )
            .await?;
        }
        if summary.cancelled {
            break;
        }
        if !object.missing_indexes.is_empty() {
            progress.stage = DataSyncStage::Finalizing;
            service.gate().update_progress(permit, progress.clone());
            create_indexes(
                service,
                prepared,
                &plan.scope.target_database,
                &object.mapping.target,
                &object.missing_indexes,
            )
            .await?;
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
                &page_spec(last_id.as_ref(), None),
            )
            .await?;
        if result.documents.is_empty() {
            return Ok(());
        }
        let next_id = document_id(
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
        insert_documents(
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
                &page_spec(last_id.as_ref(), Some(json!({"_id": 1}))),
            )
            .await?;
        if result.documents.is_empty() {
            return Ok(());
        }
        let ids: Vec<Value> = result
            .documents
            .iter()
            .map(document_id)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .cloned()
            .collect();
        last_id = ids.last().cloned();
        progress.add_scanned(ids.len() as u64);
        summary.scanned = summary.scanned.saturating_add(ids.len() as u64);

        let existing = existing_id_keys(
            service,
            &prepared.target,
            &plan.scope.target_database,
            &object.mapping.target,
            &ids,
        )
        .await?;
        let mut missing = Vec::new();
        for id in ids {
            if existing.contains(&id_key(&id)?) {
                progress.add_skipped(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                missing.push(id);
            }
        }
        let documents = fetch_documents_by_ids(
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
            ids_missing_after_fetch(&existing, &documents, result.documents.len())?;
        if missing_after_scan > 0 {
            progress.add_skipped(missing_after_scan);
            summary.skipped = summary.skipped.saturating_add(missing_after_scan);
            summary.push_warning(format!(
                "源 Collection {} 有 {missing_after_scan} 个文档在读取期间消失",
                object.mapping.source
            ));
        }
        insert_documents(
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

fn page_spec(last_id: Option<&Value>, projection: Option<Value>) -> MongoQuerySpec {
    MongoQuerySpec {
        filter: last_id
            .map(|id| json!({"_id": {"$gt": id}}))
            .unwrap_or_else(|| json!({})),
        projection,
        sort: Some(json!({"_id": 1})),
        skip: None,
        limit: Some(TRANSFER_BATCH_ITEMS as i64),
        result_byte_limit: Some(TRANSFER_BATCH_BYTES),
    }
}

async fn existing_id_keys(
    service: &DataSyncService,
    config: &ramag_domain::entities::ConnectionConfig,
    database: &str,
    collection: &str,
    ids: &[Value],
) -> Result<HashSet<Vec<u8>>> {
    let mut existing = HashSet::with_capacity(ids.len());
    for chunk in id_chunks(ids)? {
        let result = service
            .mongo_service()
            .find(
                config,
                database,
                collection,
                &MongoQuerySpec {
                    filter: json!({"_id": {"$in": chunk}}),
                    projection: Some(json!({"_id": 1})),
                    sort: None,
                    skip: None,
                    limit: Some(chunk.len() as i64),
                    result_byte_limit: Some(TRANSFER_BATCH_BYTES),
                },
            )
            .await?;
        for document in &result.documents {
            existing.insert(id_key(document_id(document)?)?);
        }
        if result.truncated {
            return Err(DomainError::QueryFailed(
                "MongoDB 目标 _id 批量查询超过内存边界，请缩小同步批次".into(),
            ));
        }
    }
    Ok(existing)
}

async fn fetch_documents_by_ids(
    service: &DataSyncService,
    config: &ramag_domain::entities::ConnectionConfig,
    database: &str,
    collection: &str,
    ids: Vec<Value>,
    permit: &DataSyncPermit,
) -> Result<Vec<MongoDocument>> {
    let mut documents = Vec::new();
    for chunk in id_chunks(&ids)? {
        let mut remaining: HashMap<Vec<u8>, Value> = chunk
            .iter()
            .map(|id| Ok((id_key(id)?, id.clone())))
            .collect::<Result<_>>()?;
        loop {
            if permit.cancellation_requested() {
                break;
            }
            if remaining.is_empty() {
                break;
            }
            let filter_ids: Vec<Value> = remaining.values().cloned().collect();
            let result = service
                .mongo_service()
                .find(
                    config,
                    database,
                    collection,
                    &MongoQuerySpec {
                        filter: json!({"_id": {"$in": filter_ids}}),
                        projection: None,
                        sort: Some(json!({"_id": 1})),
                        skip: None,
                        limit: Some(remaining.len() as i64),
                        result_byte_limit: Some(TRANSFER_BATCH_BYTES),
                    },
                )
                .await?;
            if result.documents.is_empty() {
                break;
            }
            let before = remaining.len();
            for document in result.documents {
                remaining.remove(&id_key(document_id(&document)?)?);
                documents.push(document);
            }
            if remaining.len() == before {
                return Err(DomainError::QueryFailed(
                    "MongoDB 缺失文档查询未推进".into(),
                ));
            }
            if !result.truncated {
                break;
            }
        }
    }
    Ok(documents)
}

#[allow(clippy::too_many_arguments)]
async fn insert_documents(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    database: &str,
    collection: &str,
    documents: Vec<MongoDocument>,
    permit: &DataSyncPermit,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    let mut batch = Vec::new();
    let mut batch_bytes = 0usize;
    for document in documents {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        let bytes = mongo_value_retained_bytes(&document);
        if bytes > TRANSFER_BATCH_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "MongoDB 单文档超过 {} MiB 应用传输边界",
                TRANSFER_BATCH_BYTES / 1024 / 1024
            )));
        }
        if !batch.is_empty()
            && (batch.len() >= TRANSFER_BATCH_ITEMS
                || batch_bytes.saturating_add(bytes) > TRANSFER_BATCH_BYTES)
        {
            flush_documents(
                service,
                prepared,
                database,
                collection,
                std::mem::take(&mut batch),
                batch_bytes,
                progress,
                summary,
            )
            .await?;
            batch_bytes = 0;
        }
        batch_bytes = batch_bytes.saturating_add(bytes);
        batch.push(document);
    }
    if !batch.is_empty() {
        if permit.cancellation_requested() {
            summary.cancelled = true;
            return Ok(());
        }
        flush_documents(
            service,
            prepared,
            database,
            collection,
            batch,
            batch_bytes,
            progress,
            summary,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn flush_documents(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    database: &str,
    collection: &str,
    documents: Vec<MongoDocument>,
    bytes: usize,
    progress: &mut DataSyncProgress,
    summary: &mut DataSyncSummary,
) -> Result<()> {
    progress.stage = DataSyncStage::Writing;
    let outcome = service
        .mongo_service()
        .insert_many(&prepared.target, database, collection, documents, true)
        .await?;
    progress.add_inserted(outcome.inserted);
    progress.add_skipped(outcome.duplicates);
    progress.add_bytes(bytes as u64);
    summary.inserted = summary.inserted.saturating_add(outcome.inserted);
    summary.skipped = summary.skipped.saturating_add(outcome.duplicates);
    summary.bytes = summary.bytes.saturating_add(bytes as u64);
    Ok(())
}

async fn create_collection(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    database: &str,
    collection: &str,
    blueprint: &MongoCollectionBlueprint,
) -> Result<()> {
    let mut command = blueprint
        .options
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    command.insert("create".into(), Value::String(collection.into()));
    service
        .mongo_service()
        .run_command(&prepared.target, database, Value::Object(command))
        .await?;
    Ok(())
}

async fn create_indexes(
    service: &DataSyncService,
    prepared: &PreparedDataSync,
    database: &str,
    collection: &str,
    indexes: &[Value],
) -> Result<()> {
    service
        .mongo_service()
        .run_command(
            &prepared.target,
            database,
            json!({"createIndexes": collection, "indexes": indexes}),
        )
        .await?;
    Ok(())
}

fn document_id(document: &MongoDocument) -> Result<&Value> {
    document
        .get("_id")
        .ok_or_else(|| DomainError::QueryFailed("MongoDB 文档缺少 _id".into()))
}

fn id_key(id: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(id)
        .map_err(|error| DomainError::Other(format!("序列化 MongoDB _id 失败：{error}")))
}

fn ids_missing_after_fetch(
    existing: &HashSet<Vec<u8>>,
    documents: &[MongoDocument],
    source_ids: usize,
) -> Result<u64> {
    let fetched: HashSet<Vec<u8>> = documents
        .iter()
        .map(|document| id_key(document_id(document)?))
        .collect::<Result<_>>()?;
    let accounted = existing.len().saturating_add(fetched.len());
    Ok(source_ids.saturating_sub(accounted) as u64)
}

fn id_chunks(ids: &[Value]) -> Result<Vec<&[Value]>> {
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut bytes = 0usize;
    for (index, id) in ids.iter().enumerate() {
        let id_bytes = id_key(id)?.len();
        if id_bytes > ID_FILTER_BYTES {
            return Err(DomainError::InvalidConfig(
                "MongoDB _id 过大，无法构造安全的批量查询".into(),
            ));
        }
        if index > start
            && (index - start >= TRANSFER_BATCH_ITEMS
                || bytes.saturating_add(id_bytes) > ID_FILTER_BYTES)
        {
            chunks.push(&ids[start..index]);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(id_bytes);
    }
    if start < ids.len() {
        chunks.push(&ids[start..]);
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_chunks_respect_item_and_byte_boundaries() {
        let ids: Vec<Value> = (0..=TRANSFER_BATCH_ITEMS)
            .map(|index| json!(index))
            .collect();
        let chunks = id_chunks(&ids).expect("普通 ID 应可分批");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), TRANSFER_BATCH_ITEMS);
        assert_eq!(chunks[1].len(), 1);
    }

    #[test]
    fn page_spec_uses_strict_keyset_not_offset() {
        let spec = page_spec(Some(&json!(7)), Some(json!({"_id": 1})));
        assert_eq!(spec.filter, json!({"_id": {"$gt": 7}}));
        assert_eq!(spec.sort, Some(json!({"_id": 1})));
        assert_eq!(spec.skip, None);
    }

    #[test]
    fn document_identity_is_mandatory() {
        assert_eq!(document_id(&json!({"_id": "a"})).unwrap(), &json!("a"));
        assert!(document_id(&json!({"name": "a"})).is_err());
    }
}
