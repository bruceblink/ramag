use super::*;

impl RedisExportScope {
    pub(super) fn contains(&self, key: &str) -> bool {
        match self {
            Self::Key(expected) => key == expected,
            Self::Prefix(prefix) => key
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with(':')),
        }
    }
}

pub(crate) fn parse_export_scope(header: &Value) -> Result<Option<RedisExportScope>> {
    let Some(scope) = header.get("scope").and_then(Value::as_str) else {
        return Ok(None);
    };
    let object = header
        .get("object")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::InvalidConfig("Redis 对象文件头缺少 object 字段".into()))?;
    validate_redis_key(object)?;
    match scope {
        "key" => Ok(Some(RedisExportScope::Key(object.to_string()))),
        "prefix" => Ok(Some(RedisExportScope::Prefix(object.to_string()))),
        other => Err(DomainError::InvalidConfig(format!(
            "不支持的 Redis 导出范围「{other}」"
        ))),
    }
}

pub async fn export_redis_db(
    svc: &RedisService,
    config: &ConnectionConfig,
    db: u8,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    ensure_redis(config)?;
    let total_keys = svc.db_size(config, db).await?;

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        reporter.snapshot.objects_total = Some(total_keys);
        reporter.stage("扫描 key", format!("DB {db}"));

        sink.write_str(&format!(
            "{}\n",
            json!({"ramag_export": 1, "engine": "redis", "db": db})
        ))?;

        let mut line = Vec::with_capacity(64 * 1024);
        let key_source = ExportKeySource { svc, config, db };
        let vanished = export_scanned_keys(
            &key_source,
            None,
            cancel,
            &mut sink,
            &mut line,
            &mut summary,
            &mut reporter,
        )
        .await?;
        if summary.cancelled {
            return Ok(finish_summary(summary, start));
        }
        if vanished > 0 {
            summary.push_warning(format!(
                "{vanished} 个 key 在导出期间消失（并发删除 / 过期）"
            ));
        }
        summary.bytes = sink.bytes_written();
        reporter.emit();
        sink.finish()?;
        Ok(finish_summary(summary, start))
    })
    .await
}

pub(crate) enum KeyOutcome {
    Exported,
    Vanished,
}

pub(crate) struct ExportKeySource<'a> {
    pub(crate) svc: &'a RedisService,
    pub(crate) config: &'a ConnectionConfig,
    pub(crate) db: u8,
}

pub(crate) async fn export_key(
    source: &ExportKeySource<'_>,
    key: &str,
    mut page: RedisValuePage,
    cancel: &AtomicBool,
    sink: &mut super::super::ExportSink,
    line: &mut Vec<u8>,
    summary: &mut TransferSummary,
) -> Result<KeyOutcome> {
    let Some(kind) = value_kind(&page.items) else {
        return Ok(KeyOutcome::Vanished);
    };
    let ttl_ms = page.ttl_ms.filter(|ttl| *ttl >= 0);
    if page.ttl_ms == Some(-2) {
        return Ok(KeyOutcome::Vanished);
    }
    let mut skipped_items = 0u64;
    let mut first = true;
    loop {
        if is_cancelled(cancel) {
            summary.cancelled = true;
            return Ok(KeyOutcome::Exported);
        }
        skipped_items += page.skipped;
        let (fragment, count) = encode_fragment(&page.items)?;
        summary.items += count;
        write_fragment_records(sink, line, key, kind, ttl_ms, &mut first, fragment)?;
        let Some(next) = page.next.clone() else { break };
        page = source
            .svc
            .read_value_page(source.config, source.db, key, Some(kind), next, PAGE_ITEMS)
            .await?;
    }
    if skipped_items > 0 {
        summary.push_warning(format!(
            "key {key}：{skipped_items} 个条目无法表达（二进制 field），已跳过"
        ));
    }
    Ok(KeyOutcome::Exported)
}

/// 将 Redis 页切为不超过 32 MiB 的完整 JSONL 记录；条目不拆分。
fn write_fragment_records(
    sink: &mut super::super::ExportSink,
    line: &mut Vec<u8>,
    key: &str,
    kind: RedisType,
    ttl_ms: Option<i64>,
    first: &mut bool,
    fragment: Value,
) -> Result<()> {
    let Value::Array(items) = fragment else {
        return write_fragment_record(sink, line, key, kind, ttl_ms, first, fragment);
    };
    if items.is_empty() {
        return write_fragment_record(
            sink,
            line,
            key,
            kind,
            ttl_ms,
            first,
            Value::Array(Vec::new()),
        );
    }

    let mut group = Vec::new();
    let mut group_bytes = 2usize; // JSON 数组的 []
    let first_budget = fragment_value_budget(key, kind, ttl_ms, true)?;
    let continuation_budget = fragment_value_budget(key, kind, ttl_ms, false)?;
    for item in items {
        let item_bytes = serde_json::to_vec(&item)
            .map_err(|error| DomainError::Storage(format!("序列化 Redis 条目失败：{error}")))?
            .len();
        let separator = usize::from(!group.is_empty());
        let budget = if *first {
            first_budget
        } else {
            continuation_budget
        };
        if !group.is_empty()
            && group_bytes
                .saturating_add(separator)
                .saturating_add(item_bytes)
                > budget
        {
            write_fragment_record(
                sink,
                line,
                key,
                kind,
                ttl_ms,
                first,
                Value::Array(std::mem::take(&mut group)),
            )?;
            group_bytes = 2;
        }
        let budget = if *first {
            first_budget
        } else {
            continuation_budget
        };
        let separator = usize::from(!group.is_empty());
        let next_bytes = group_bytes
            .saturating_add(separator)
            .saturating_add(item_bytes);
        if next_bytes > budget {
            return Err(DomainError::InvalidConfig(format!(
                "key {key} 的单个条目超过 {} MiB 传输上限，无法导出",
                TRANSFER_BATCH_BYTES / 1024 / 1024
            )));
        }
        group.push(item);
        group_bytes = next_bytes;
    }
    if !group.is_empty() {
        write_fragment_record(sink, line, key, kind, ttl_ms, first, Value::Array(group))?;
    }
    Ok(())
}

fn fragment_value_budget(
    key: &str,
    kind: RedisType,
    ttl_ms: Option<i64>,
    first: bool,
) -> Result<usize> {
    let empty_record = fragment_record(key, kind, ttl_ms, first, Value::Array(Vec::new()));
    let empty_bytes = serde_json::to_vec(&empty_record)
        .map_err(|error| DomainError::Storage(format!("序列化 Redis 导出记录失败：{error}")))?
        .len();
    // 预算扣除记录头、数组括号与换行。
    let fixed_bytes = empty_bytes.saturating_sub(2).saturating_add(1);
    TRANSFER_BATCH_BYTES
        .checked_sub(fixed_bytes)
        .ok_or_else(|| DomainError::InvalidConfig(format!("key {key} 导出记录头超过传输上限")))
}

fn write_fragment_record(
    sink: &mut super::super::ExportSink,
    line: &mut Vec<u8>,
    key: &str,
    kind: RedisType,
    ttl_ms: Option<i64>,
    first: &mut bool,
    fragment: Value,
) -> Result<()> {
    let record = fragment_record(key, kind, ttl_ms, *first, fragment);
    write_json_line(sink, line, &record)?;
    *first = false;
    Ok(())
}

fn fragment_record(
    key: &str,
    kind: RedisType,
    ttl_ms: Option<i64>,
    first: bool,
    fragment: Value,
) -> Value {
    if first {
        json!({
            "key": key,
            "type": kind_name(kind),
            "ttl_ms": ttl_ms,
            "value": fragment,
        })
    } else {
        json!({"key": key, "more": true, "value": fragment})
    }
}

/// 按可选 MATCH 模式扫描并导出 key；整库与前缀导出共用同一条批量首页热路径。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn export_scanned_keys(
    source: &ExportKeySource<'_>,
    pattern: Option<&str>,
    cancel: &AtomicBool,
    sink: &mut super::super::ExportSink,
    line: &mut Vec<u8>,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<u64> {
    let mut cursor = 0u64;
    let mut vanished = 0u64;
    loop {
        if is_cancelled(cancel) {
            summary.cancelled = true;
            break;
        }
        let batch = source
            .svc
            .scan_batch(source.config, source.db, cursor, pattern, None, SCAN_BATCH)
            .await?;
        for key_batch in batch.keys.chunks(MAX_REDIS_VALUE_PAGE_BATCH) {
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(vanished);
            }
            let keys = key_batch
                .iter()
                .map(|metadata| metadata.key.clone())
                .collect::<Vec<_>>();
            let pages = source
                .svc
                .read_value_first_pages(source.config, source.db, &keys, PAGE_ITEMS)
                .await?;
            if pages.len() != key_batch.len() {
                return Err(DomainError::QueryFailed(format!(
                    "Redis 批量首页数量不一致：{} / {}",
                    pages.len(),
                    key_batch.len()
                )));
            }
            for (metadata, page) in key_batch.iter().zip(pages) {
                if is_cancelled(cancel) {
                    summary.cancelled = true;
                    return Ok(vanished);
                }
                match export_key(source, &metadata.key, page, cancel, sink, line, summary).await? {
                    KeyOutcome::Exported => summary.objects += 1,
                    KeyOutcome::Vanished => vanished += 1,
                }
                reporter.snapshot.objects_done += 1;
                reporter.snapshot.items_done = summary.items;
                reporter.snapshot.bytes = sink.bytes_written();
                reporter.emit_every(PROGRESS_EVERY_KEYS);
            }
        }
        cursor = batch.cursor;
        if cursor == 0 {
            break;
        }
    }
    Ok(vanished)
}
