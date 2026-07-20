//! 查询与写操作：find / count / aggregate / insert_one / update_one / delete_one / run_command / ping。
//! 全部走最小 API；options 通过 builder 链式装载

use std::time::Instant;

use bson::{Bson, Document, doc};
use futures::TryStreamExt;
use mongodb::Client;
use ramag_domain::entities::{
    InsertManyOutcome, MAX_MONGO_DOCUMENT_BYTES, MongoDocument, MongoQueryResult, MongoQuerySpec,
};
use ramag_domain::error::{DomainError, Result};
use serde_json::Value;

use crate::errors::map_mongo_error;
use crate::types::{document_to_json, json_to_document};

/// 单次结果集安全上限，避免把超大集合完整装入内存。
const MAX_RESULT_DOCS: usize = 50_000;
const MAX_RESULT_BSON_BYTES: usize = 32 * 1024 * 1024;

/// `ping` 命令，仅用于 test_connection
pub async fn ping(client: &Client) -> Result<()> {
    client
        .database("admin")
        .run_command(doc! {"ping": 1})
        .await
        .map_err(map_mongo_error)?;
    Ok(())
}

/// `buildInfo.version`
pub async fn server_version(client: &Client) -> Result<String> {
    let r: Document = client
        .database("admin")
        .run_command(doc! {"buildInfo": 1})
        .await
        .map_err(map_mongo_error)?;
    Ok(r.get_str("version")
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "unknown".to_string()))
}

pub async fn find(
    client: &Client,
    db: &str,
    coll: &str,
    spec: &MongoQuerySpec,
) -> Result<MongoQueryResult> {
    let start = Instant::now();

    let filter_doc = if spec.filter.is_null() {
        Document::new()
    } else {
        json_to_document(spec.filter.clone())?
    };
    let sort_doc = optional_document(spec.sort.as_ref())?;
    let projection_doc = optional_document(spec.projection.as_ref())?;
    ensure_command_document_budget(
        [&filter_doc]
            .into_iter()
            .chain(sort_doc.iter())
            .chain(projection_doc.iter()),
        "MongoDB find",
    )?;

    let collection = client.database(db).collection::<Document>(coll);
    let mut find_action = collection
        .find(filter_doc)
        .limit(effective_find_limit(spec.limit));

    if let Some(skip) = spec.skip {
        find_action = find_action.skip(skip);
    }
    if let Some(doc) = sort_doc {
        find_action = find_action.sort(doc);
    }
    if let Some(doc) = projection_doc {
        find_action = find_action.projection(doc);
    }

    let mut cursor = find_action.await.map_err(map_mongo_error)?;
    let mut docs: Vec<MongoDocument> = Vec::new();
    let mut budget = ResultBudget::default();
    let mut truncated = false;
    while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
        let bytes = document_size(&doc)?;
        if !budget.try_reserve(bytes, MAX_RESULT_DOCS, MAX_RESULT_BSON_BYTES) {
            truncated = true;
            break;
        }
        docs.push(document_to_json(doc));
    }
    warn_if_truncated(truncated, &budget, "find");

    let elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(MongoQueryResult::read_maybe_truncated(
        docs, elapsed_ms, truncated,
    ))
}

pub async fn count(client: &Client, db: &str, coll: &str, filter: MongoDocument) -> Result<u64> {
    let filter_doc = if filter.is_null() {
        Document::new()
    } else {
        json_to_document(filter)?
    };
    ensure_command_document_budget([&filter_doc], "MongoDB count")?;
    let collection = client.database(db).collection::<Document>(coll);
    let n = collection
        .count_documents(filter_doc)
        .await
        .map_err(map_mongo_error)?;
    Ok(n)
}

pub async fn aggregate(
    client: &Client,
    db: &str,
    coll: &str,
    pipeline: Vec<MongoDocument>,
) -> Result<MongoQueryResult> {
    let start = Instant::now();
    let mut docs_pipeline: Vec<Document> = Vec::with_capacity(pipeline.len());
    let mut command_bytes = 0usize;
    for stage in pipeline {
        let document = json_to_document(stage)?;
        command_bytes = reserve_command_document_bytes(
            command_bytes,
            &document,
            "MongoDB aggregate",
            MAX_MONGO_DOCUMENT_BYTES,
        )?;
        docs_pipeline.push(document);
    }
    let collection = client.database(db).collection::<Document>(coll);
    let mut cursor = collection
        .aggregate(docs_pipeline)
        .await
        .map_err(map_mongo_error)?;
    let mut out = Vec::new();
    let mut budget = ResultBudget::default();
    let mut truncated = false;
    while let Some(d) = cursor.try_next().await.map_err(map_mongo_error)? {
        let bytes = document_size(&d)?;
        if !budget.try_reserve(bytes, MAX_RESULT_DOCS, MAX_RESULT_BSON_BYTES) {
            truncated = true;
            break;
        }
        out.push(document_to_json(d));
    }
    warn_if_truncated(truncated, &budget, "aggregate");
    let elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(MongoQueryResult::read_maybe_truncated(
        out, elapsed_ms, truncated,
    ))
}

pub async fn insert_one(
    client: &Client,
    db: &str,
    coll: &str,
    document: MongoDocument,
) -> Result<String> {
    let doc = json_to_document(document)?;
    ensure_command_document_budget([&doc], "MongoDB insert")?;
    let collection = client.database(db).collection::<Document>(coll);
    let r = collection.insert_one(doc).await.map_err(map_mongo_error)?;
    Ok(format_bson_id(&r.inserted_id))
}

/// MongoDB duplicate key 错误码（E11000）
const DUPLICATE_KEY_CODE: i32 = 11000;

/// 批量插入。`skip_duplicates=true` 走无序批量：重复 `_id`（E11000）不算错误只计数；
/// 其余 write error / write concern error 照常报错。false 走有序批量，任何错误即失败
pub async fn insert_many(
    client: &Client,
    db: &str,
    coll: &str,
    documents: Vec<MongoDocument>,
    skip_duplicates: bool,
) -> Result<InsertManyOutcome> {
    let mut docs: Vec<Document> = Vec::with_capacity(documents.len());
    for document in documents {
        docs.push(json_to_document(document)?);
    }
    ensure_command_document_budget(docs.iter(), "MongoDB insertMany")?;
    let attempted = docs.len() as u64;
    if attempted == 0 {
        return Ok(InsertManyOutcome::default());
    }
    let collection = client.database(db).collection::<Document>(coll);
    match collection.insert_many(docs).ordered(!skip_duplicates).await {
        Ok(result) => Ok(InsertManyOutcome {
            inserted: result.inserted_ids.len() as u64,
            duplicates: 0,
        }),
        Err(error) if skip_duplicates => match duplicates_only(&error) {
            Some(duplicates) => Ok(InsertManyOutcome {
                inserted: attempted.saturating_sub(duplicates),
                duplicates,
            }),
            None => Err(map_mongo_error(error)),
        },
        Err(error) => Err(map_mongo_error(error)),
    }
}

/// 错误若纯由重复 `_id` 组成则返回重复条数，否则 None（按真错误上抛）
fn duplicates_only(error: &mongodb::error::Error) -> Option<u64> {
    let mongodb::error::ErrorKind::InsertMany(bulk) = error.kind.as_ref() else {
        return None;
    };
    if bulk.write_concern_error.is_some() {
        return None;
    }
    let write_errors = bulk.write_errors.as_ref()?;
    if write_errors.is_empty()
        || write_errors
            .iter()
            .any(|error| error.code != DUPLICATE_KEY_CODE)
    {
        return None;
    }
    Some(write_errors.len() as u64)
}

pub async fn update_one(
    client: &Client,
    db: &str,
    coll: &str,
    filter: MongoDocument,
    update: MongoDocument,
) -> Result<MongoQueryResult> {
    let start = Instant::now();
    let filter_doc = json_to_document(filter)?;
    let update_doc = json_to_document(update)?;
    ensure_command_document_budget([&filter_doc, &update_doc], "MongoDB update")?;
    let collection = client.database(db).collection::<Document>(coll);
    let r = collection
        .update_one(filter_doc, update_doc)
        .await
        .map_err(map_mongo_error)?;
    tracing::info!(
        coll = coll,
        matched = r.matched_count,
        modified = r.modified_count,
        "mongo update_one done"
    );
    let elapsed_ms = start.elapsed().as_millis() as u64;
    // affected 取 matched_count（定位到的文档数）而非 modified_count：改成与原值相同时
    // modified=0，用 matched 才能正确反映「已定位」，避免上层把「值未变」误报成「未匹配」
    Ok(MongoQueryResult::write(
        r.matched_count,
        elapsed_ms,
        "updateOne",
    ))
}

pub async fn delete_one(
    client: &Client,
    db: &str,
    coll: &str,
    filter: MongoDocument,
) -> Result<MongoQueryResult> {
    let start = Instant::now();
    let filter_doc = json_to_document(filter)?;
    ensure_command_document_budget([&filter_doc], "MongoDB delete")?;
    let collection = client.database(db).collection::<Document>(coll);
    let r = collection
        .delete_one(filter_doc)
        .await
        .map_err(map_mongo_error)?;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(MongoQueryResult::write(
        r.deleted_count,
        elapsed_ms,
        "deleteOne",
    ))
}

/// 兜底任意命令。例：`dbStats` / `serverStatus` / `createIndexes`。
/// 游标类命令（find / aggregate / listCollections / listIndexes）改走驱动游标
/// (`run_cursor_command`)：由驱动正确处理 getMore + 连接钉定。此前用独立 run_command 手动发
/// getMore 不钉连接，对需要多批次的结果（单条文档较大、一个 16MB batch 装不下，几十条也会触发）
/// 会卡死/超时——这正是某些集合在本工具打不开、在别的客户端却正常的根因
pub async fn run_command(
    client: &Client,
    db: &str,
    command: MongoDocument,
) -> Result<MongoDocument> {
    let cmd_doc = json_to_document(command)?;
    ensure_command_document_budget([&cmd_doc], "MongoDB command")?;
    if is_cursor_command(&cmd_doc) {
        return collect_cursor_command(client, db, cmd_doc).await;
    }
    let raw: Document = client
        .database(db)
        .run_command(cmd_doc)
        .await
        .map_err(map_mongo_error)?;
    Ok(document_to_json(raw))
}

/// 命令是否返回游标（含这些命令名时需用游标抽取完整结果）
fn is_cursor_command(cmd: &Document) -> bool {
    ["find", "aggregate", "listCollections", "listIndexes"]
        .iter()
        .any(|k| cmd.contains_key(*k))
}

/// 用驱动游标执行命令并收集结果（受统一安全上限保护），
/// 包成 `cursor.firstBatch` 形态供上层 `parse_run_command_response` 解析
async fn collect_cursor_command(client: &Client, db: &str, cmd: Document) -> Result<MongoDocument> {
    let mut cursor = client
        .database(db)
        .run_cursor_command(cmd)
        .await
        .map_err(map_mongo_error)?;
    let mut docs: Vec<Bson> = Vec::new();
    let mut budget = ResultBudget::default();
    let mut truncated = false;
    while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
        let bytes = document_size(&doc)?;
        if !budget.try_reserve(bytes, MAX_RESULT_DOCS, MAX_RESULT_BSON_BYTES) {
            truncated = true;
            break;
        }
        docs.push(Bson::Document(doc));
    }
    warn_if_truncated(truncated, &budget, "runCommand");
    // 内部标记：截断信息随 firstBatch 一起上传，parse_run_command_response 提取后剔除
    let resp = doc! {
        "cursor": { "firstBatch": Bson::Array(docs), "id": 0i64 },
        "__ramag_truncated": truncated,
        "ok": 1.0,
    };
    Ok(document_to_json(resp))
}

fn optional_document(value: Option<&Value>) -> Result<Option<Document>> {
    value.cloned().map(json_to_document).transpose()
}

fn effective_find_limit(limit: Option<i64>) -> i64 {
    const SAFETY_LIMIT: i64 = MAX_RESULT_DOCS as i64 + 1;
    match limit {
        Some(value) if value != 0 && value.unsigned_abs() <= SAFETY_LIMIT as u64 => value,
        Some(value) if value < 0 => -SAFETY_LIMIT,
        _ => SAFETY_LIMIT,
    }
}

fn ensure_command_document_budget<'a>(
    documents: impl IntoIterator<Item = &'a Document>,
    label: &str,
) -> Result<()> {
    let mut total = 0usize;
    for document in documents {
        total = reserve_command_document_bytes(total, document, label, MAX_MONGO_DOCUMENT_BYTES)?;
    }
    Ok(())
}

fn reserve_command_document_bytes(
    current: usize,
    document: &Document,
    label: &str,
    limit: usize,
) -> Result<usize> {
    let bytes = document_size(document)?;
    let total = current
        .checked_add(bytes)
        .ok_or_else(|| DomainError::InvalidConfig(format!("{label} BSON 总长度溢出")))?;
    if total > limit {
        return Err(DomainError::InvalidConfig(format!(
            "{label} BSON 超过 {} MiB 上限",
            limit / 1024 / 1024
        )));
    }
    Ok(total)
}

#[derive(Debug, Default)]
struct ResultBudget {
    documents: usize,
    bson_bytes: usize,
}

impl ResultBudget {
    /// 返回 false 表示本次文档超出任一上限；调用方只有实际读到额外文档时才标记截断。
    fn try_reserve(&mut self, bytes: usize, max_documents: usize, max_bytes: usize) -> bool {
        let Some(total_bytes) = self.bson_bytes.checked_add(bytes) else {
            return false;
        };
        if self.documents >= max_documents || total_bytes > max_bytes {
            return false;
        }
        self.documents += 1;
        self.bson_bytes = total_bytes;
        true
    }
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn document_size(document: &Document) -> Result<usize> {
    let mut writer = CountingWriter::default();
    document.to_writer(&mut writer).map_err(|error| {
        ramag_domain::error::DomainError::Other(format!("计算 MongoDB 文档大小失败：{error}"))
    })?;
    Ok(writer.bytes)
}

fn warn_if_truncated(truncated: bool, budget: &ResultBudget, operation: &'static str) {
    if !truncated {
        return;
    }
    tracing::warn!(
        collected = budget.documents,
        bson_bytes = budget.bson_bytes,
        operation,
        "mongo cursor truncated at safety cap"
    );
}

/// insertedId 是 Bson，常见 ObjectId / String / Int64；统一转可读字符串
fn format_bson_id(b: &Bson) -> String {
    let v: Value = b.clone().into_relaxed_extjson();
    match &v {
        Value::String(s) => s.clone(),
        Value::Object(map) => {
            if let Some(oid) = map.get("$oid").and_then(|x| x.as_str()) {
                return oid.to_string();
            }
            v.to_string()
        }
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;
    use serde_json::json;

    #[test]
    fn optional_query_document_rejects_non_object() {
        assert!(optional_document(Some(&json!([1, 2]))).is_err());
        assert!(optional_document(Some(&json!({"created_at": -1}))).is_ok());
        assert!(optional_document(None).unwrap().is_none());
    }

    #[test]
    fn result_budget_only_reports_actual_overflow() {
        let mut budget = ResultBudget::default();
        assert!(budget.try_reserve(4, 2, 8));
        assert!(budget.try_reserve(4, 2, 8));
        assert_eq!(budget.documents, 2);
        assert_eq!(budget.bson_bytes, 8);

        assert!(!budget.try_reserve(1, 2, 8));
        assert_eq!(budget.documents, 2);
        assert_eq!(budget.bson_bytes, 8);
    }

    #[test]
    fn result_budget_rejects_byte_overflow_before_count_limit() {
        let mut budget = ResultBudget::default();
        assert!(budget.try_reserve(6, 10, 8));
        assert!(!budget.try_reserve(3, 10, 8));
        assert_eq!(budget.documents, 1);
        assert_eq!(budget.bson_bytes, 6);
    }

    #[test]
    fn document_size_matches_bson_encoding() {
        let document = bson::doc! { "name": "ramag", "count": 3 };
        assert_eq!(
            document_size(&document).unwrap(),
            bson::to_vec(&document).unwrap().len()
        );
    }

    #[test]
    fn find_limit_is_capped_server_side_but_preserves_small_signed_limits() {
        assert_eq!(effective_find_limit(None), 50_001);
        assert_eq!(effective_find_limit(Some(0)), 50_001);
        assert_eq!(effective_find_limit(Some(100)), 100);
        assert_eq!(effective_find_limit(Some(-100)), -100);
        assert_eq!(effective_find_limit(Some(100_000)), 50_001);
        assert_eq!(effective_find_limit(Some(-100_000)), -50_001);
    }

    #[test]
    fn command_bson_budget_accepts_boundary_and_rejects_overflow() {
        let document = bson::doc! { "a": "b" };
        let bytes = document_size(&document).unwrap();
        assert_eq!(
            reserve_command_document_bytes(0, &document, "test", bytes).unwrap(),
            bytes
        );
        assert!(reserve_command_document_bytes(1, &document, "test", bytes).is_err());
    }

    #[test]
    fn format_objectid_extracts_hex() {
        let oid = ObjectId::new();
        let formatted = format_bson_id(&Bson::ObjectId(oid));
        assert_eq!(formatted.len(), 24);
        assert!(formatted.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn format_string_id_passthrough() {
        let v = Bson::String("custom-id".into());
        assert_eq!(format_bson_id(&v), "custom-id");
    }
}
