use super::*;
use tracing::{info, warn};

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
    info!(
        operation = "mongo_export",
        connection_id = %config.id,
        db,
        target = target_collection.unwrap_or("*"),
        path = %path.display(),
        "transfer started"
    );
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

    let result = with_export_sink(path, |mut sink| async move {
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
    .await;
    match &result {
        Ok(summary) => info!(
            operation = "mongo_export",
            connection_id = %config.id,
            db,
            target = target_collection.unwrap_or("*"),
            path = %path.display(),
            objects = summary.objects,
            items = summary.items,
            failed = summary.failed,
            cancelled = summary.cancelled,
            elapsed_ms = summary.elapsed_ms,
            "transfer finished"
        ),
        Err(error) => warn!(
            operation = "mongo_export",
            connection_id = %config.id,
            db,
            target = target_collection.unwrap_or("*"),
            path = %path.display(),
            error = %error,
            "transfer failed"
        ),
    }
    result
}
