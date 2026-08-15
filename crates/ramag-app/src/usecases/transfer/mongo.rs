//! MongoDB 结构化导入导出。

mod export;
pub use export::{export_mongo_collection, export_mongo_database};
mod import_support;
mod jsonl;
pub use jsonl::import_jsonl_into_collection;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MongoQuerySpec, ProgressFn, TRANSFER_BATCH_BYTES,
    TRANSFER_BATCH_ITEMS, TransferSummary, validate_mongo_collection_name,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{
    Reporter, finish_summary, is_cancelled, read_line_bounded, with_export_sink, write_json_line,
};
use crate::usecases::MongoService;

const PAGE_DOCS: i64 = TRANSFER_BATCH_ITEMS as i64;
const IMPORT_BATCH_DOCS: usize = TRANSFER_BATCH_ITEMS;
const IMPORT_BATCH_BYTES: usize = TRANSFER_BATCH_BYTES;
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;

/// 将单集合结构化文件恢复到所选数据库；文件中的集合名保持不变。
pub async fn import_mongo_collection(
    svc: &MongoService,
    config: &ConnectionConfig,
    path: &Path,
    target_db: &str,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let header = import_support::read_export_header(path).map_err(|error| {
        warn!(
            operation = "mongo_import",
            connection_id = %config.id,
            path = %path.display(),
            stage = "validate_collection_scope",
            error = %error,
            "collection import header rejected"
        );
        error
    })?;
    let scope = import_support::scoped_collection(&header).map_err(|error| {
        warn!(
            operation = "mongo_import",
            connection_id = %config.id,
            path = %path.display(),
            stage = "validate_collection_scope",
            error = %error,
            "collection import scope rejected"
        );
        error
    })?;
    if scope.is_none() {
        let error = DomainError::InvalidConfig("请选择由 Ramag“导出此集合”生成的单集合文件".into());
        warn!(
            operation = "mongo_import",
            connection_id = %config.id,
            path = %path.display(),
            stage = "validate_collection_scope",
            error = %error,
            "collection import rejected"
        );
        return Err(error);
    }
    import_mongo_database(svc, config, path, Some(target_db), policy, cancel, progress).await
}

pub async fn import_mongo_database(
    svc: &MongoService,
    config: &ConnectionConfig,
    path: &Path,
    target_db: Option<&str>,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let requested_database = target_db.unwrap_or("<from export file>");
    let result =
        import_mongo_database_inner(svc, config, path, target_db, policy, cancel, progress).await;
    if let Err(error) = &result {
        warn!(
            operation = "mongo_import",
            connection_id = %config.id,
            requested_database,
            policy = ?policy,
            path = %path.display(),
            error = %error,
            "transfer failed"
        );
    }
    result
}

async fn import_mongo_database_inner(
    svc: &MongoService,
    config: &ConnectionConfig,
    path: &Path,
    target_db: Option<&str>,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_mongo(config)?;
    if config.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);

    let mut summary = TransferSummary::default();
    let mut reporter = Reporter::new(progress);

    let mut header_line = String::new();
    read_line_bounded(
        &mut reader,
        &mut header_line,
        MAX_LINE_BYTES,
        "MongoDB 导入文件",
    )?;
    let header: Value = serde_json::from_str(header_line.trim())
        .map_err(|_| DomainError::InvalidConfig("文件首行不是有效的导出头".into()))?;
    if header.get("ramag_export").and_then(Value::as_u64) != Some(1)
        || header.get("engine").and_then(Value::as_str) != Some("mongodb")
    {
        return Err(DomainError::InvalidConfig(
            "文件不是 MongoDB 导出（engine 不匹配）".into(),
        ));
    }
    let scoped_collection = import_support::scoped_collection(&header)?.map(str::to_string);
    let file_db = header
        .get("database")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::InvalidConfig("文件头缺少 database 字段".into()))?
        .to_string();
    let db = target_db.unwrap_or(&file_db).to_string();
    info!(
        operation = "mongo_import",
        connection_id = %config.id,
        database = %db,
        policy = ?policy,
        path = %path.display(),
        "transfer started"
    );

    let existing: std::collections::HashSet<String> = svc
        .list_collections(config, &db)
        .await?
        .into_iter()
        .map(|collection| collection.name)
        .collect();

    let mut current: Option<import_support::CollectionCtx> = None;
    let mut seen_collections = std::collections::HashSet::new();
    let mut line = String::new();
    loop {
        let read = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES, "MongoDB 导入文件")?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(trimmed)
            .map_err(|error| DomainError::InvalidConfig(format!("JSONL 解析失败：{error}")))?;

        if let Some(name) = record.get("collection").and_then(Value::as_str) {
            if scoped_collection
                .as_deref()
                .is_some_and(|expected| name != expected)
            {
                return Err(DomainError::InvalidConfig(format!(
                    "单集合文件声明了范围外集合「{name}」"
                )));
            }
            if let Some(ctx) = current.take() {
                import_support::finalize_collection(
                    svc,
                    config,
                    &db,
                    ctx,
                    &mut summary,
                    &mut reporter,
                )
                .await?;
            }
            if is_cancelled(cancel) {
                summary.cancelled = true;
                let summary = finish_summary(summary, start);
                info!(
                    operation = "mongo_import",
                    connection_id = %config.id,
                    database = %db,
                    objects = summary.objects,
                    items = summary.items,
                    failed = summary.failed,
                    cancelled = true,
                    elapsed_ms = summary.elapsed_ms,
                    "transfer finished"
                );
                return Ok(summary);
            }
            validate_mongo_collection_name(name)?;
            if !seen_collections.insert(name.to_string()) {
                return Err(DomainError::InvalidConfig(format!(
                    "集合 {name} 在导入文件中重复声明"
                )));
            }
            let indexes = match record.get("indexes") {
                Some(Value::Array(indexes)) => indexes.clone(),
                None => Vec::new(),
                Some(_) => {
                    return Err(DomainError::InvalidConfig(format!(
                        "集合 {name} 的 indexes 不是数组"
                    )));
                }
            };
            let options = match record.get("options") {
                Some(Value::Object(options)) => options.clone(),
                None => serde_json::Map::new(),
                Some(_) => {
                    return Err(DomainError::InvalidConfig(format!(
                        "集合 {name} 的 options 不是对象"
                    )));
                }
            };
            let mut ctx = import_support::CollectionCtx::new(name.to_string(), indexes);
            reporter.stage("导入集合", name);
            let mut should_create = !existing.contains(name);
            if existing.contains(name) {
                match policy {
                    ConflictPolicy::Skip => {
                        ctx.skip = true;
                        summary.skipped += 1;
                    }
                    // 合并时保留集合，通过跳过重复 _id 补齐文档。
                    ConflictPolicy::Merge => should_create = false,
                    ConflictPolicy::Fail => {
                        return Err(DomainError::QueryFailed(format!(
                            "集合「{name}」已存在（冲突策略：报错停止）"
                        )));
                    }
                    ConflictPolicy::Overwrite => {
                        should_create = true;
                        if let Err(error) =
                            svc.run_command(config, &db, json!({"drop": name})).await
                        {
                            // 目标并发消失视为已删除，其余错误按对象失败处理。
                            if !error.message().contains("NamespaceNotFound") {
                                ctx.skip = true;
                                summary.failed += 1;
                                summary.push_warning(format!(
                                    "覆盖删除集合 {name} 失败：{}",
                                    error.message()
                                ));
                            }
                        }
                    }
                }
            }
            if !ctx.skip && should_create {
                reporter.stage("重建集合结构", name);
                import_support::create_collection(svc, config, &db, name, options).await?;
            }
            current = Some(ctx);
            continue;
        }

        if let Some(doc) = record.get("doc") {
            let Some(ctx) = current.as_mut() else {
                return Err(DomainError::InvalidConfig(
                    "文档行出现在任何集合声明之前，文件损坏".into(),
                ));
            };
            if ctx.skip {
                continue;
            }
            if trimmed.len() > IMPORT_BATCH_BYTES {
                return Err(DomainError::InvalidConfig(format!(
                    "集合 {} 的单个文档记录超过 {} MiB，无法导入",
                    ctx.name,
                    IMPORT_BATCH_BYTES / 1024 / 1024
                )));
            }
            if !ctx.batch.is_empty()
                && (ctx.batch.len() >= IMPORT_BATCH_DOCS
                    || ctx.batch_bytes.saturating_add(trimmed.len()) > IMPORT_BATCH_BYTES)
            {
                import_support::flush_batch(svc, config, &db, ctx, &mut summary).await?;
                reporter.snapshot.items_done = summary.items;
                reporter.emit();
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    let summary = finish_summary(summary, start);
                    info!(
                        operation = "mongo_import",
                        connection_id = %config.id,
                        database = %db,
                        objects = summary.objects,
                        items = summary.items,
                        failed = summary.failed,
                        cancelled = true,
                        elapsed_ms = summary.elapsed_ms,
                        "transfer finished"
                    );
                    return Ok(summary);
                }
            }
            ctx.batch_bytes = ctx.batch_bytes.saturating_add(trimmed.len());
            ctx.batch.push(doc.clone());
            if ctx.batch.len() >= IMPORT_BATCH_DOCS {
                import_support::flush_batch(svc, config, &db, ctx, &mut summary).await?;
                reporter.snapshot.items_done = summary.items;
                reporter.emit();
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    let summary = finish_summary(summary, start);
                    info!(
                        operation = "mongo_import",
                        connection_id = %config.id,
                        database = %db,
                        objects = summary.objects,
                        items = summary.items,
                        failed = summary.failed,
                        cancelled = true,
                        elapsed_ms = summary.elapsed_ms,
                        "transfer finished"
                    );
                    return Ok(summary);
                }
            }
            continue;
        }
        summary.push_warning(format!(
            "无法识别的记录被跳过：{}",
            &trimmed[..trimmed.len().min(80)]
        ));
    }
    if let Some(ctx) = current.take() {
        import_support::finalize_collection(svc, config, &db, ctx, &mut summary, &mut reporter)
            .await?;
    }
    if let Some(expected) = scoped_collection
        && !seen_collections.contains(&expected)
    {
        return Err(DomainError::InvalidConfig(format!(
            "单集合文件缺少集合「{expected}」的结构声明"
        )));
    }
    reporter.emit();
    let summary = finish_summary(summary, start);
    info!(
        operation = "mongo_import",
        connection_id = %config.id,
        database = %db,
        objects = summary.objects,
        items = summary.items,
        failed = summary.failed,
        cancelled = summary.cancelled,
        elapsed_ms = summary.elapsed_ms,
        "transfer finished"
    );
    Ok(summary)
}

fn ensure_mongo(config: &ConnectionConfig) -> Result<()> {
    if config.driver != DriverKind::Mongodb {
        return Err(DomainError::InvalidConfig(
            "该操作仅支持 MongoDB 连接".into(),
        ));
    }
    Ok(())
}

/// `_id` keyset 过滤：$expr 聚合比较是 BSON 跨类型全序，配合 sort {_id:1} 不漏文档；
/// $literal 防止字符串 _id（如 "$foo"）被当作字段路径
fn keyset_filter(last_id: &Option<Value>) -> Value {
    match last_id {
        None => Value::Null,
        Some(id) => json!({"$expr": {"$gt": ["$_id", {"$literal": id}]}}),
    }
}

/// `listCollections` 的 options 可直接作为 `create` 命令选项，保留 capped、validator、
/// collation、time-series 等集合级结构。
async fn export_collection_options(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    target_collection: Option<&str>,
) -> Result<std::collections::HashMap<String, serde_json::Map<String, Value>>> {
    let command = match target_collection {
        Some(collection) => {
            json!({"listCollections": 1, "filter": {"name": collection}, "nameOnly": false})
        }
        None => json!({"listCollections": 1, "nameOnly": false}),
    };
    let response = svc.run_command(config, db, command).await?;
    let specs = response
        .pointer("/cursor/firstBatch")
        .and_then(Value::as_array)
        .ok_or_else(|| DomainError::QueryFailed("集合创建选项响应缺少 firstBatch".into()))?;
    let mut options = std::collections::HashMap::with_capacity(specs.len());
    for spec in specs {
        let name = spec
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| DomainError::QueryFailed("listCollections 响应缺少集合名称".into()))?;
        let value = spec
            .get("options")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| DomainError::QueryFailed(format!("集合 {name} 的 options 响应无效")))?;
        options.insert(name.to_string(), value);
    }
    Ok(options)
}

/// 导出索引 spec：剔除自动的 _id_ 索引与 ns 字段（历史版本残留）。
/// 索引属于集合结构，读取失败必须终止，不能生成看似成功但不可完整恢复的文件。
async fn export_indexes(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    coll: &str,
) -> Result<Vec<Value>> {
    let response = svc
        .run_command(config, db, json!({"listIndexes": coll}))
        .await?;
    if response
        .pointer("/cursor/firstBatch")
        .and_then(Value::as_array)
        .is_none()
    {
        return Err(DomainError::QueryFailed(format!(
            "集合 {coll} 的索引响应缺少 firstBatch"
        )));
    }
    Ok(filter_index_specs(&response))
}

fn filter_index_specs(response: &Value) -> Vec<Value> {
    response
        .pointer("/cursor/firstBatch")
        .and_then(Value::as_array)
        .map(|specs| {
            specs
                .iter()
                .filter(|spec| spec.get("name").and_then(Value::as_str) != Some("_id_"))
                .map(|spec| {
                    let mut spec = spec.clone();
                    if let Some(map) = spec.as_object_mut() {
                        map.remove("ns");
                    }
                    spec
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
