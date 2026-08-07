use super::*;

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
        // 仅清空文档，保留集合定义和索引。
        reporter.stage("清空集合", format!("{db}.{coll}"));
        svc.run_command(
            config,
            db,
            json!({"delete": coll, "deletes": [{"q": {}, "limit": 0}]}),
        )
        .await?;
    }
    reporter.stage("导入文档", format!("{db}.{coll}"));

    // `Fail` 遇到重复 _id 即停止，其他策略跳过并计数。
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

/// 提交集合导入批次；`Fail` 遇错即停，其他策略记录警告后继续。
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
