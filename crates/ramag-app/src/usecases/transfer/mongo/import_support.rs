//! MongoDB 结构化导入辅助逻辑。

use super::*;

pub(super) fn read_export_header(path: &Path) -> Result<Value> {
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

pub(super) fn scoped_collection(header: &Value) -> Result<Option<&str>> {
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

pub(super) struct CollectionCtx {
    pub(super) name: String,
    pub(super) indexes: Vec<Value>,
    pub(super) batch: Vec<Value>,
    pub(super) batch_bytes: usize,
    duplicates: u64,
    pub(super) skip: bool,
}

impl CollectionCtx {
    pub(super) fn new(name: String, indexes: Vec<Value>) -> Self {
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

pub(super) async fn flush_batch(
    service: &MongoService,
    config: &ConnectionConfig,
    database: &str,
    context: &mut CollectionCtx,
    summary: &mut TransferSummary,
) -> Result<()> {
    if context.batch.is_empty() {
        return Ok(());
    }
    let documents = std::mem::take(&mut context.batch);
    context.batch_bytes = 0;
    let outcome = service
        .insert_many(config, database, &context.name, documents, true)
        .await?;
    summary.items += outcome.inserted;
    context.duplicates += outcome.duplicates;
    Ok(())
}

pub(super) async fn create_collection(
    service: &MongoService,
    config: &ConnectionConfig,
    database: &str,
    name: &str,
    options: serde_json::Map<String, Value>,
) -> Result<()> {
    service
        .run_command(config, database, create_collection_command(name, options))
        .await
        .map(|_| ())
        .map_err(|error| {
            DomainError::QueryFailed(format!(
                "重建集合 {name} 的创建选项失败：{}",
                error.message()
            ))
        })
}

pub(super) fn create_collection_command(
    name: &str,
    options: serde_json::Map<String, Value>,
) -> Value {
    // MongoDB 命令名必须是首字段。
    let mut command = serde_json::Map::new();
    command.insert("create".to_string(), Value::String(name.to_string()));
    for (key, value) in options {
        if key != "create" {
            command.insert(key, value);
        }
    }
    Value::Object(command)
}

pub(super) async fn finalize_collection(
    service: &MongoService,
    config: &ConnectionConfig,
    database: &str,
    mut context: CollectionCtx,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    if context.skip {
        return Ok(());
    }
    flush_batch(service, config, database, &mut context, summary).await?;
    if context.duplicates > 0 {
        summary.push_warning(format!(
            "集合 {}：{} 条重复 _id 已跳过",
            context.name, context.duplicates
        ));
    }
    if !context.indexes.is_empty() {
        reporter.stage("重建索引", &context.name);
        let command = json!({"createIndexes": context.name, "indexes": context.indexes});
        service
            .run_command(config, database, command)
            .await
            .map_err(|error| {
                DomainError::QueryFailed(format!(
                    "集合 {} 重建索引失败：{}",
                    context.name,
                    error.message()
                ))
            })?;
    }
    summary.objects += 1;
    reporter.snapshot.objects_done += 1;
    reporter.emit();
    Ok(())
}
