//! MongoDB 按库导出 / 导入（JSONL）。
//!
//! 文件格式（每行一个 JSON 对象）：
//! - 首行 `{"ramag_export":1,"engine":"mongodb","database":"..."}`
//! - 集合行 `{"collection":"users","indexes":[<原始索引 spec>…]}`（不含 _id_ 索引）
//! - 文档行 `{"doc":{…}}`（Extended JSON，Int64 保 $numberLong，与查询面板同一映射）
//!
//! 导出翻页用 `$expr + $literal` 的 `_id` keyset：聚合比较是跨类型全序，
//! 混合类型 `_id` 也不漏（skip/limit 的 O(n²) 与漂移都规避掉）。
//! 导入固定无序批量 + 重复 `_id` 计数：目标集合在检查后被并发建出也不会误报失败

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MongoQuerySpec, ProgressFn, TransferSummary,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use serde_json::{Value, json};

use super::{Reporter, finish_summary, is_cancelled, with_export_sink, write_json_line};
use crate::usecases::MongoService;

/// 单页文档数（driver 单次上限 5 万 / 32 MiB，留足余量）
const PAGE_DOCS: i64 = 2_000;
/// 导入批：条数与字节预算（BSON 单文档上限 16 MiB，批预算给驱动留一半）
const IMPORT_BATCH_DOCS: usize = 500;
const IMPORT_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

pub async fn export_mongo_database(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_mongo(config)?;
    let collections = svc.list_collections(config, db).await?;

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        let data_collections: Vec<_> = collections.iter().filter(|c| !c.is_view).collect();
        reporter.snapshot.objects_total = Some(data_collections.len() as u64);
        for view in collections.iter().filter(|c| c.is_view) {
            summary.push_warning(format!("视图 {} 不导出（仅数据集合）", view.name));
        }

        sink.write_str(&format!(
            "{}\n",
            json!({"ramag_export": 1, "engine": "mongodb", "database": db})
        ))?;

        let mut line = Vec::with_capacity(64 * 1024);
        for collection in data_collections {
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            let name = collection.name.as_str();
            reporter.stage("导出集合", name);

            let indexes = export_indexes(svc, config, db, name, &mut summary).await;
            write_json_line(
                &mut sink,
                &mut line,
                &json!({"collection": name, "indexes": indexes}),
            )?;

            let mut last_id: Option<Value> = None;
            loop {
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    return Ok(finish_summary(summary, start));
                }
                let spec = MongoQuerySpec {
                    filter: keyset_filter(&last_id),
                    projection: None,
                    sort: Some(json!({"_id": 1})),
                    skip: None,
                    limit: Some(PAGE_DOCS),
                };
                let result = svc.find(config, db, name, &spec).await?;
                if result.documents.is_empty() {
                    break;
                }
                let page_len = result.documents.len() as u64;
                let next_id = result
                    .documents
                    .last()
                    .and_then(|doc| doc.get("_id").cloned());
                for doc in result.documents {
                    write_json_line(&mut sink, &mut line, &json!({"doc": doc}))?;
                }
                summary.items += page_len;
                reporter.snapshot.items_done += page_len;
                reporter.snapshot.bytes = sink.bytes_written();
                reporter.emit();
                match next_id {
                    Some(id) => last_id = Some(id),
                    None => {
                        // 无 _id 的文档无法 keyset 续读（仅特殊系统集合会出现）
                        summary
                            .push_warning(format!("集合 {name} 存在无 _id 文档，仅导出到该批为止"));
                        break;
                    }
                }
            }
            summary.objects += 1;
            reporter.snapshot.objects_done += 1;
            reporter.emit();
        }

        summary.bytes = sink.bytes_written();
        sink.finish()?;
        Ok(finish_summary(summary, start))
    })
    .await
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
    use std::io::BufRead as _;

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

    // 首行文件头
    let mut header_line = String::new();
    reader
        .read_line(&mut header_line)
        .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
    let header: Value = serde_json::from_str(header_line.trim())
        .map_err(|_| DomainError::InvalidConfig("文件首行不是有效的导出头".into()))?;
    if header.get("engine").and_then(Value::as_str) != Some("mongodb") {
        return Err(DomainError::InvalidConfig(
            "文件不是 MongoDB 导出（engine 不匹配）".into(),
        ));
    }
    let file_db = header
        .get("database")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::InvalidConfig("文件头缺少 database 字段".into()))?
        .to_string();
    let db = target_db.unwrap_or(&file_db).to_string();

    let existing: std::collections::HashSet<String> = svc
        .list_collections(config, &db)
        .await
        .map(|cols| cols.into_iter().map(|c| c.name).collect())
        .unwrap_or_default();

    // 当前集合状态机
    let mut current: Option<CollectionCtx> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(DomainError::InvalidConfig(
                "导入文件单行超长，疑似损坏".into(),
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(trimmed)
            .map_err(|error| DomainError::InvalidConfig(format!("JSONL 解析失败：{error}")))?;

        if let Some(name) = record.get("collection").and_then(Value::as_str) {
            if let Some(ctx) = current.take() {
                finalize_collection(svc, config, &db, ctx, &mut summary, &mut reporter).await?;
            }
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            let indexes = record
                .get("indexes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut ctx = CollectionCtx::new(name.to_string(), indexes);
            reporter.stage("导入集合", name);
            if existing.contains(name) {
                match policy {
                    ConflictPolicy::Skip => {
                        ctx.skip = true;
                        summary.skipped += 1;
                    }
                    // 合并：保留集合，靠无序批量的重复 _id 跳过实现条目级补齐
                    ConflictPolicy::Merge => {}
                    ConflictPolicy::Fail => {
                        return Err(DomainError::QueryFailed(format!(
                            "集合「{name}」已存在（冲突策略：报错停止）"
                        )));
                    }
                    ConflictPolicy::Overwrite => {
                        if let Err(error) =
                            svc.run_command(config, &db, json!({"drop": name})).await
                        {
                            // 目标并发消失视作已删；其余错误按对象失败处理
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
            ctx.batch_bytes += trimmed.len();
            ctx.batch.push(doc.clone());
            if ctx.batch.len() >= IMPORT_BATCH_DOCS || ctx.batch_bytes >= IMPORT_BATCH_BYTES {
                flush_batch(svc, config, &db, ctx, &mut summary).await?;
                reporter.snapshot.items_done = summary.items;
                reporter.emit();
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    return Ok(finish_summary(summary, start));
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
        finalize_collection(svc, config, &db, ctx, &mut summary, &mut reporter).await?;
    }
    reporter.emit();
    Ok(finish_summary(summary, start))
}

struct CollectionCtx {
    name: String,
    indexes: Vec<Value>,
    batch: Vec<Value>,
    batch_bytes: usize,
    inserted_any: bool,
    duplicates: u64,
    skip: bool,
}

impl CollectionCtx {
    fn new(name: String, indexes: Vec<Value>) -> Self {
        Self {
            name,
            indexes,
            batch: Vec::with_capacity(IMPORT_BATCH_DOCS),
            batch_bytes: 0,
            inserted_any: false,
            duplicates: 0,
            skip: false,
        }
    }
}

async fn flush_batch(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    ctx: &mut CollectionCtx,
    summary: &mut TransferSummary,
) -> Result<()> {
    if ctx.batch.is_empty() {
        return Ok(());
    }
    let documents = std::mem::take(&mut ctx.batch);
    ctx.batch_bytes = 0;
    let outcome = svc
        .insert_many(config, db, &ctx.name, documents, true)
        .await?;
    summary.items += outcome.inserted;
    ctx.duplicates += outcome.duplicates;
    ctx.inserted_any = true;
    Ok(())
}

/// 集合收尾：冲洗余批、建索引、空集合显式 create
async fn finalize_collection(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    mut ctx: CollectionCtx,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    if ctx.skip {
        return Ok(());
    }
    flush_batch(svc, config, db, &mut ctx, summary).await?;
    if ctx.duplicates > 0 {
        summary.push_warning(format!(
            "集合 {}：{} 条重复 _id 已跳过",
            ctx.name, ctx.duplicates
        ));
    }
    if !ctx.indexes.is_empty() {
        reporter.stage("重建索引", &ctx.name);
        let command = json!({"createIndexes": ctx.name, "indexes": ctx.indexes});
        if let Err(error) = svc.run_command(config, db, command).await {
            summary.push_warning(format!("集合 {} 建索引失败：{}", ctx.name, error.message()));
        }
    } else if !ctx.inserted_any {
        // 空集合且无索引：显式 create，保留「空集合」这个事实
        if let Err(error) = svc
            .run_command(config, db, json!({"create": ctx.name}))
            .await
            && !error.message().contains("NamespaceExists")
        {
            summary.push_warning(format!("创建空集合 {} 失败：{}", ctx.name, error.message()));
        }
    }
    summary.objects += 1;
    reporter.snapshot.objects_done += 1;
    reporter.emit();
    Ok(())
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

/// 导出索引 spec：剔除自动的 _id_ 索引与 ns 字段（历史版本残留）
async fn export_indexes(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    coll: &str,
    summary: &mut TransferSummary,
) -> Vec<Value> {
    let response = match svc
        .run_command(config, db, json!({"listIndexes": coll}))
        .await
    {
        Ok(response) => response,
        Err(error) => {
            summary.push_warning(format!("集合 {coll} 索引读取失败：{}", error.message()));
            return Vec::new();
        }
    };
    filter_index_specs(&response)
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

/// 集合级裸 JSONL 导入：每行一个文档（兼容 Extended JSON），与结果区导出配对。
/// Skip=重复 _id 跳过、Overwrite=先清空集合文档（保留索引）再导入、Fail=遇重复即停；
/// Merge 与文档级 Skip 重合，调用方不提供该选项
pub async fn import_jsonl_into_collection(
    svc: &MongoService,
    config: &ConnectionConfig,
    target: (&str, &str),
    path: &Path,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    use std::io::BufRead as _;

    let start = Instant::now();
    let (db, coll) = target;
    ensure_mongo(config)?;
    if config.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);

    let mut summary = TransferSummary {
        objects: 1,
        ..Default::default()
    };
    let mut reporter = Reporter::new(progress);
    reporter.snapshot.objects_total = Some(1);

    if policy == ConflictPolicy::Overwrite {
        // 清空文档而非 drop：保留集合定义与索引
        reporter.stage("清空集合", format!("{db}.{coll}"));
        svc.run_command(
            config,
            db,
            json!({"delete": coll, "deletes": [{"q": {}, "limit": 0}]}),
        )
        .await?;
    }
    reporter.stage("导入文档", format!("{db}.{coll}"));

    // Fail 策略严格插入：重复 _id 直接报错停止；其余策略跳过重复并计数
    let skip_duplicates = policy != ConflictPolicy::Fail;
    let mut duplicates: u64 = 0;
    let mut batch: Vec<Value> = Vec::with_capacity(IMPORT_BATCH_DOCS);
    let mut batch_bytes = 0usize;
    let mut batch_first_line = 0u64;
    let mut line = String::new();
    let mut line_no: u64 = 0;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        reporter.snapshot.bytes += read as u64;
        if line.len() > MAX_LINE_BYTES {
            return Err(DomainError::InvalidConfig(
                "导入文件单行超长，疑似损坏".into(),
            ));
        }
        if is_cancelled(cancel) {
            summary.cancelled = true;
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let document: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                summary.failed += 1;
                summary.push_warning(format!("第 {line_no} 行 JSON 解析失败：{error}"));
                continue;
            }
        };
        if !document.is_object() {
            summary.failed += 1;
            summary.push_warning(format!("第 {line_no} 行不是 JSON 文档，已跳过"));
            continue;
        }
        if batch.is_empty() {
            batch_first_line = line_no;
        }
        batch_bytes += trimmed.len();
        batch.push(document);
        if batch.len() >= IMPORT_BATCH_DOCS || batch_bytes >= IMPORT_BATCH_BYTES {
            flush_jsonl_batch(
                svc,
                config,
                (db, coll),
                skip_duplicates,
                policy,
                &mut batch,
                batch_first_line,
                &mut duplicates,
                &mut summary,
                &mut reporter,
            )
            .await?;
            batch_bytes = 0;
        }
    }
    if !summary.cancelled {
        flush_jsonl_batch(
            svc,
            config,
            (db, coll),
            skip_duplicates,
            policy,
            &mut batch,
            batch_first_line,
            &mut duplicates,
            &mut summary,
            &mut reporter,
        )
        .await?;
    }
    if duplicates > 0 {
        summary.skipped += duplicates;
        summary.push_warning(format!("{duplicates} 条重复 _id 已跳过"));
    }
    Ok(finish_summary(summary, start))
}

/// 冲洗集合级导入批：Fail 策略错误即停；其余策略计失败 + 告警后继续
#[allow(clippy::too_many_arguments)]
async fn flush_jsonl_batch(
    svc: &MongoService,
    config: &ConnectionConfig,
    target: (&str, &str),
    skip_duplicates: bool,
    policy: ConflictPolicy,
    batch: &mut Vec<Value>,
    first_line: u64,
    duplicates: &mut u64,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let (db, coll) = target;
    let count = batch.len() as u64;
    let documents = std::mem::take(batch);
    match svc
        .insert_many(config, db, coll, documents, skip_duplicates)
        .await
    {
        Ok(outcome) => {
            summary.items += outcome.inserted;
            *duplicates += outcome.duplicates;
        }
        Err(error) => {
            if policy == ConflictPolicy::Fail {
                return Err(error);
            }
            summary.failed += count;
            summary.push_warning(format!(
                "自第 {first_line} 行起的 {count} 条批次插入失败：{}",
                error.message()
            ));
        }
    }
    reporter.snapshot.items_done = summary.items;
    reporter.emit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyset_filter_wraps_literal() {
        assert!(keyset_filter(&None).is_null());
        let filter = keyset_filter(&Some(json!({"$oid": "0123456789abcdef01234567"})));
        assert_eq!(
            filter
                .pointer("/$expr/$gt/1/$literal/$oid")
                .and_then(Value::as_str),
            Some("0123456789abcdef01234567")
        );
        // 字符串 _id 以 $ 开头也被 $literal 保护
        let tricky = keyset_filter(&Some(json!("$field")));
        assert_eq!(
            tricky
                .pointer("/$expr/$gt/1/$literal")
                .and_then(Value::as_str),
            Some("$field")
        );
    }

    #[test]
    fn index_specs_drop_id_index_and_ns() {
        let response = json!({"cursor": {"firstBatch": [
            {"name": "_id_", "key": {"_id": 1}},
            {"name": "email_1", "key": {"email": 1}, "unique": true, "ns": "db.users"},
        ]}});
        let specs = filter_index_specs(&response);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0]["name"], "email_1");
        assert!(specs[0].get("ns").is_none());
        assert_eq!(filter_index_specs(&json!({})).len(), 0);
    }
}
