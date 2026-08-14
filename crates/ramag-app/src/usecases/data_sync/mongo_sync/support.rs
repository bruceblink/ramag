//! MongoDB 同步的查询、批处理与建结构辅助逻辑。

use std::collections::{HashMap, HashSet};

use ramag_domain::entities::{
    ConnectionConfig, DataSyncProgress, DataSyncStage, DataSyncSummary, MongoDocument,
    MongoQuerySpec, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, mongo_value_retained_bytes,
};
use ramag_domain::error::{DomainError, Result};
use serde_json::{Map, Value, json};

use super::super::gate::DataSyncPermit;
use super::super::service::{DataSyncService, MongoCollectionBlueprint, PreparedDataSync};

const ID_FILTER_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn page_spec(last_id: Option<&Value>, projection: Option<Value>) -> MongoQuerySpec {
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

pub(super) async fn existing_id_keys(
    service: &DataSyncService,
    config: &ConnectionConfig,
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

pub(super) async fn fetch_documents_by_ids(
    service: &DataSyncService,
    config: &ConnectionConfig,
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
            if permit.cancellation_requested() || remaining.is_empty() {
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
pub(super) async fn insert_documents(
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

pub(super) async fn create_collection(
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

pub(super) async fn create_indexes(
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

pub(super) fn ids_missing_after_fetch(
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

pub(super) fn document_id(document: &MongoDocument) -> Result<&Value> {
    document
        .get("_id")
        .ok_or_else(|| DomainError::QueryFailed("MongoDB 文档缺少 _id".into()))
}

pub(super) fn id_key(id: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(id)
        .map_err(|error| DomainError::Other(format!("序列化 MongoDB _id 失败：{error}")))
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
