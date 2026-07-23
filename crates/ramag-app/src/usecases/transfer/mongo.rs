//! MongoDB 按库 / 单集合结构化导出与导入（JSONL）。
//!
//! 文件格式（每行一个 JSON 对象）：
//! - 首行 `{"ramag_export":1,"engine":"mongodb","database":"..."}`
//! - 集合行 `{"collection":"users","options":{…},"indexes":[…]}`；options 可恢复
//!   capped / validator / collation / time-series 等创建语义，索引不重复记录自动 `_id_`
//! - 文档行 `{"doc":{…}}`（Extended JSON，Int64 保 $numberLong，与查询面板同一映射）
//!
//! 导出翻页用 `$expr + $literal` 的 `_id` keyset：聚合比较是跨类型全序，
//! 混合类型 `_id` 也不漏（skip/limit 的 O(n²) 与漂移都规避掉）。
//! 导入固定无序批量 + 重复 `_id` 计数：目标集合在检查后被并发建出也不会误报失败

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MongoQuerySpec, ProgressFn, TRANSFER_BATCH_BYTES,
    TRANSFER_BATCH_ITEMS, TransferSummary, validate_mongo_collection_name,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use serde_json::{Value, json};

use super::{
    Reporter, finish_summary, is_cancelled, read_line_bounded, with_export_sink, write_json_line,
};
use crate::usecases::MongoService;

/// 导出与导入统一使用 5,000 条 / 32 MiB 批次。
const PAGE_DOCS: i64 = TRANSFER_BATCH_ITEMS as i64;
const IMPORT_BATCH_DOCS: usize = TRANSFER_BATCH_ITEMS;
const IMPORT_BATCH_BYTES: usize = TRANSFER_BATCH_BYTES;
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;

pub async fn export_mongo_database(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    export_mongo(svc, config, db, None, path, cancel, progress).await
}

/// 导出单集合创建选项、索引与全部文档；文件可直接走 MongoDB 结构化导入恢复。
pub async fn export_mongo_collection(
    svc: &MongoService,
    config: &ConnectionConfig,
    target: (&str, &str),
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let (db, collection) = target;
    export_mongo(svc, config, db, Some(collection), path, cancel, progress).await
}

async fn export_mongo(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    target_collection: Option<&str>,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_mongo(config)?;
    // 集合清单与完整创建选项并发读取；整库场景只发一次 listCollections，避免逐集合往返。
    let (collections, collection_options) = futures::try_join!(
        svc.list_collections(config, db),
        export_collection_options(svc, config, db, target_collection),
    )?;
    let (collection_names, skipped_views): (Vec<String>, Vec<String>) = match target_collection {
        Some(name) => {
            let collection = collections
                .iter()
                .find(|item| item.name == name)
                .ok_or_else(|| DomainError::NotFound(format!("集合 {db}.{name} 不存在")))?;
            if collection.is_view {
                return Err(DomainError::InvalidConfig(
                    "MongoDB 视图不支持集合级结构与数据导出，请使用查询结果导出".into(),
                ));
            }
            (vec![name.to_string()], Vec::new())
        }
        None => (
            collections
                .iter()
                .filter(|collection| !collection.is_view)
                .map(|collection| collection.name.clone())
                .collect(),
            collections
                .iter()
                .filter(|collection| collection.is_view)
                .map(|collection| collection.name.clone())
                .collect(),
        ),
    };

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        reporter.snapshot.objects_total = Some(collection_names.len() as u64);
        for view in skipped_views {
            summary.push_warning(format!("视图 {view} 不导出（仅真实集合可完整恢复）"));
        }

        let header = match target_collection {
            Some(collection) => json!({
                "ramag_export": 1,
                "engine": "mongodb",
                "database": db,
                "scope": "collection",
                "object": collection,
            }),
            None => json!({"ramag_export": 1, "engine": "mongodb", "database": db}),
        };
        sink.write_str(&format!("{header}\n"))?;

        let mut line = Vec::with_capacity(64 * 1024);
        for name in &collection_names {
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            reporter.stage("导出集合结构", name);

            let options = collection_options
                .get(name)
                .cloned()
                .ok_or_else(|| DomainError::NotFound(format!("集合 {db}.{name} 在导出期间消失")))?;
            let indexes = export_indexes(svc, config, db, name).await?;
            write_json_line(
                &mut sink,
                &mut line,
                &json!({"collection": name, "options": options, "indexes": indexes}),
            )?;
            reporter.stage("导出集合数据", name);

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
                    result_byte_limit: Some(ramag_domain::entities::TRANSFER_BATCH_BYTES),
                };
                let result = svc.find(config, db, name, &spec).await?;
                if result.documents.is_empty() {
                    if result.truncated {
                        return Err(DomainError::InvalidConfig(format!(
                            "集合 {name} 的单个文档超过 {} MiB 传输上限，无法导出",
                            TRANSFER_BATCH_BYTES / 1024 / 1024
                        )));
                    }
                    break;
                }
                let page_len = result.documents.len() as u64;
                let next_id = result
                    .documents
                    .last()
                    .and_then(|doc| doc.get("_id").cloned())
                    .ok_or_else(|| {
                        DomainError::QueryFailed(format!(
                            "集合 {db}.{name} 返回了缺少 _id 的文档，无法保证完整导出"
                        ))
                    })?;
                for doc in result.documents {
                    write_json_line(&mut sink, &mut line, &json!({"doc": doc}))?;
                }
                summary.items += page_len;
                reporter.snapshot.items_done += page_len;
                reporter.snapshot.bytes = sink.bytes_written();
                reporter.emit();
                last_id = Some(next_id);
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
    let header = read_export_header(path)?;
    if scoped_collection(&header)?.is_none() {
        return Err(DomainError::InvalidConfig(
            "请选择由 Ramag“导出此集合”生成的单集合文件".into(),
        ));
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
    let scoped_collection = scoped_collection(&header)?.map(str::to_string);
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
                finalize_collection(svc, config, &db, ctx, &mut summary, &mut reporter).await?;
            }
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
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
            let mut ctx = CollectionCtx::new(name.to_string(), indexes);
            reporter.stage("导入集合", name);
            let mut should_create = !existing.contains(name);
            if existing.contains(name) {
                match policy {
                    ConflictPolicy::Skip => {
                        ctx.skip = true;
                        summary.skipped += 1;
                    }
                    // 合并：保留集合，靠无序批量的重复 _id 跳过实现条目级补齐
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
            if !ctx.skip && should_create {
                reporter.stage("重建集合结构", name);
                create_collection(svc, config, &db, name, options).await?;
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
                flush_batch(svc, config, &db, ctx, &mut summary).await?;
                reporter.snapshot.items_done = summary.items;
                reporter.emit();
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    return Ok(finish_summary(summary, start));
                }
            }
            ctx.batch_bytes = ctx.batch_bytes.saturating_add(trimmed.len());
            ctx.batch.push(doc.clone());
            if ctx.batch.len() >= IMPORT_BATCH_DOCS {
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
    if let Some(expected) = scoped_collection
        && !seen_collections.contains(&expected)
    {
        return Err(DomainError::InvalidConfig(format!(
            "单集合文件缺少集合「{expected}」的结构声明"
        )));
    }
    reporter.emit();
    Ok(finish_summary(summary, start))
}

fn read_export_header(path: &Path) -> Result<Value> {
    use std::io::BufRead as _;

    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
    serde_json::from_str(line.trim())
        .map_err(|_| DomainError::InvalidConfig("文件首行不是有效的导出头".into()))
}

fn scoped_collection(header: &Value) -> Result<Option<&str>> {
    match header.get("scope").and_then(Value::as_str) {
        None => Ok(None),
        Some("collection") => {
            let name = header
                .get("object")
                .and_then(Value::as_str)
                .ok_or_else(|| DomainError::InvalidConfig("单集合文件头缺少 object 字段".into()))?;
            validate_mongo_collection_name(name)?;
            Ok(Some(name))
        }
        Some(scope) => Err(DomainError::InvalidConfig(format!(
            "不支持的 MongoDB 导出范围「{scope}」"
        ))),
    }
}

struct CollectionCtx {
    name: String,
    indexes: Vec<Value>,
    batch: Vec<Value>,
    batch_bytes: usize,
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
    Ok(())
}

async fn create_collection(
    svc: &MongoService,
    config: &ConnectionConfig,
    db: &str,
    name: &str,
    options: serde_json::Map<String, Value>,
) -> Result<()> {
    let command = create_collection_command(name, options);
    svc.run_command(config, db, command)
        .await
        .map(|_| ())
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "重建集合 {name} 的创建选项失败：{}",
                error.message()
            ))
        })
}

fn create_collection_command(name: &str, options: serde_json::Map<String, Value>) -> Value {
    // MongoDB 以 BSON 文档首字段识别命令名；必须先放 create，再追加选项。
    let mut command = serde_json::Map::new();
    command.insert("create".to_string(), Value::String(name.to_string()));
    for (key, value) in options {
        if key != "create" {
            command.insert(key, value);
        }
    }
    Value::Object(command)
}

/// 集合收尾：冲洗余批并重建索引。集合本身已在读入声明时按创建选项建立。
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
        svc.run_command(config, db, command)
            .await
            .map_err(|error| {
                DomainError::QueryFailed(format!(
                    "集合 {} 重建索引失败：{}",
                    ctx.name,
                    error.message()
                ))
            })?;
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
        let read = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES, "MongoDB JSONL 文件")?;
        if read == 0 {
            break;
        }
        line_no += 1;
        reporter.snapshot.bytes += read as u64;
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
        if trimmed.len() > IMPORT_BATCH_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "第 {line_no} 行超过 {} MiB，无法导入",
                IMPORT_BATCH_BYTES / 1024 / 1024
            )));
        }
        if !batch.is_empty()
            && (batch.len() >= IMPORT_BATCH_DOCS
                || batch_bytes.saturating_add(trimmed.len()) > IMPORT_BATCH_BYTES)
        {
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
            batch_first_line = line_no;
        }
        batch_bytes = batch_bytes.saturating_add(trimmed.len());
        batch.push(document);
        if batch.len() >= IMPORT_BATCH_DOCS {
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

    #[test]
    fn create_command_keeps_command_name_first() {
        let options = json!({"capped": true, "size": 1024})
            .as_object()
            .cloned()
            .unwrap();
        let command = create_collection_command("events", options);
        let fields = command.as_object().unwrap().keys().collect::<Vec<_>>();
        assert_eq!(fields.first().map(|field| field.as_str()), Some("create"));
        assert_eq!(command["create"], "events");
        assert_eq!(command["capped"], true);
    }

    #[test]
    fn collection_scope_requires_valid_object_and_rejects_other_scopes() {
        let header = json!({"scope": "collection", "object": "events"});
        assert_eq!(scoped_collection(&header).unwrap(), Some("events"));
        assert!(scoped_collection(&json!({"scope": "collection"})).is_err());
        assert!(scoped_collection(&json!({"scope": "database"})).is_err());
        assert_eq!(scoped_collection(&json!({})).unwrap(), None);
    }
}
