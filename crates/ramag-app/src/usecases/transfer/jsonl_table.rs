//! 表级 JSONL 导入：按目标表列匹配每行对象并分批插入。
//! 缺少的列使用数据库默认值，未知键会被忽略并汇总警告。
//! `Merge` 在行级等同于 `Skip`。

mod sql;

use std::collections::{BTreeSet, HashSet};
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, ProgressFn, Query, TRANSFER_BATCH_BYTES,
    TRANSFER_BATCH_ITEMS, TransferSummary,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use tracing::{info, warn};

use super::{Reporter, finish_summary, is_cancelled, read_line_bounded};
use crate::usecases::ConnectionService;
use sql::{build_insert_sql, qualified_table, render_row};

const BATCH_ROWS: usize = TRANSFER_BATCH_ITEMS;
const BATCH_BYTES: usize = TRANSFER_BATCH_BYTES;
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;
const MAX_UNKNOWN_KEYS_LISTED: usize = 8;

pub async fn import_jsonl_into_table(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    target: (&str, &str),
    path: &Path,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let (schema, table) = target;
    info!(
        operation = "sql_table_jsonl_import",
        connection_id = %config.id,
        driver = ?config.driver,
        schema,
        table,
        policy = ?policy,
        path = %path.display(),
        "transfer started"
    );
    let result =
        import_jsonl_into_table_inner(svc, config, target, path, policy, cancel, progress).await;
    match &result {
        Ok(summary) => info!(
            operation = "sql_table_jsonl_import",
            connection_id = %config.id,
            driver = ?config.driver,
            schema,
            table,
            policy = ?policy,
            path = %path.display(),
            objects = summary.objects,
            items = summary.items,
            bytes = summary.bytes,
            failed = summary.failed,
            skipped = summary.skipped,
            cancelled = summary.cancelled,
            warning_count = summary.warnings.len() as u64 + summary.warnings_overflow,
            elapsed_ms = summary.elapsed_ms,
            "transfer completed"
        ),
        Err(error) => warn!(
            operation = "sql_table_jsonl_import",
            connection_id = %config.id,
            driver = ?config.driver,
            schema,
            table,
            policy = ?policy,
            path = %path.display(),
            error = %error,
            "transfer failed"
        ),
    }
    result
}

async fn import_jsonl_into_table_inner(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    target: (&str, &str),
    path: &Path,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    let (schema, table) = target;
    if config.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if !matches!(
        config.driver,
        DriverKind::Mysql | DriverKind::Postgres | DriverKind::Sqlite
    ) {
        return Err(DomainError::InvalidConfig(
            ".jsonl 表级导入仅支持 MySQL / PostgreSQL / SQLite 连接".into(),
        ));
    }
    let column_names: Vec<String> = svc
        .list_columns(config, schema, table)
        .await?
        .into_iter()
        .map(|column| column.name)
        .collect();
    if column_names.is_empty() {
        return Err(DomainError::NotFound(format!(
            "表 {schema}.{table} 不存在或无列信息"
        )));
    }
    let column_set: HashSet<String> = column_names.iter().cloned().collect();

    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let mut summary = TransferSummary {
        objects: 1,
        ..Default::default()
    };
    let mut reporter = Reporter::new(progress);
    reporter.snapshot.objects_total = Some(1);
    let qualified = qualified_table(config.driver, schema, table);

    if policy == ConflictPolicy::Overwrite {
        // 使用 DELETE 保留普通 DML 语义，使外键冲突正常暴露。
        reporter.stage("清空表", format!("{schema}.{table}"));
        run_sql(svc, config, schema, &format!("DELETE FROM {qualified}")).await?;
    }
    reporter.stage("导入数据", format!("{schema}.{table}"));

    let mut line = String::new();
    let mut line_no: u64 = 0;
    let mut unknown_keys: BTreeSet<String> = BTreeSet::new();
    // 同列集的行组成多行 INSERT，列集变化时先提交当前批次。
    let mut batch_cols: Vec<String> = Vec::new();
    let mut batch_rows: Vec<String> = Vec::new();
    let mut batch_bytes = 0usize;
    let mut batch_first_line = 0u64;

    loop {
        let read = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES, "导入文件")?;
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
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                summary.failed += 1;
                summary.push_warning(format!("第 {line_no} 行 JSON 解析失败：{error}"));
                continue;
            }
        };
        let Some(object) = parsed.as_object() else {
            summary.failed += 1;
            summary.push_warning(format!("第 {line_no} 行不是 JSON 对象，已跳过"));
            continue;
        };
        let present = present_columns(&column_names, &column_set, object, &mut unknown_keys);
        if present.is_empty() {
            summary.failed += 1;
            summary.push_warning(format!("第 {line_no} 行没有任何键匹配表列"));
            continue;
        }
        if present != batch_cols && !batch_rows.is_empty() {
            flush_batch(
                svc,
                config,
                schema,
                &qualified,
                policy,
                &batch_cols,
                &mut batch_rows,
                batch_first_line,
                &mut summary,
                &mut reporter,
            )
            .await?;
            batch_bytes = 0;
        }
        if batch_rows.is_empty() {
            batch_cols = present;
            batch_first_line = line_no;
        }
        let tuple = render_row(config.driver, &batch_cols, object);
        let single_sql_bytes = build_insert_sql(
            config.driver,
            policy,
            &qualified,
            &batch_cols,
            std::slice::from_ref(&tuple),
        )
        .len();
        if single_sql_bytes > BATCH_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "第 {line_no} 行生成的单条 SQL 超过 {} MiB，无法导入",
                BATCH_BYTES / 1024 / 1024
            )));
        }
        let prospective_bytes = if batch_rows.is_empty() {
            single_sql_bytes
        } else {
            batch_bytes.saturating_add(2).saturating_add(tuple.len())
        };
        if !batch_rows.is_empty()
            && (batch_rows.len() >= BATCH_ROWS || prospective_bytes > BATCH_BYTES)
        {
            flush_batch(
                svc,
                config,
                schema,
                &qualified,
                policy,
                &batch_cols,
                &mut batch_rows,
                batch_first_line,
                &mut summary,
                &mut reporter,
            )
            .await?;
            batch_bytes = 0;
            batch_first_line = line_no;
        }
        batch_bytes = if batch_rows.is_empty() {
            single_sql_bytes
        } else {
            batch_bytes.saturating_add(2).saturating_add(tuple.len())
        };
        batch_rows.push(tuple);
        if batch_rows.len() >= BATCH_ROWS {
            flush_batch(
                svc,
                config,
                schema,
                &qualified,
                policy,
                &batch_cols,
                &mut batch_rows,
                batch_first_line,
                &mut summary,
                &mut reporter,
            )
            .await?;
            batch_bytes = 0;
        }
    }
    if !summary.cancelled {
        flush_batch(
            svc,
            config,
            schema,
            &qualified,
            policy,
            &batch_cols,
            &mut batch_rows,
            batch_first_line,
            &mut summary,
            &mut reporter,
        )
        .await?;
    }

    if !unknown_keys.is_empty() {
        let mut listed: Vec<&str> = unknown_keys
            .iter()
            .take(MAX_UNKNOWN_KEYS_LISTED)
            .map(String::as_str)
            .collect();
        if unknown_keys.len() > MAX_UNKNOWN_KEYS_LISTED {
            listed.push("…");
        }
        summary.push_warning(format!(
            "忽略未匹配表列的键（{} 个）：{}",
            unknown_keys.len(),
            listed.join("、")
        ));
    }
    Ok(finish_summary(summary, start))
}

/// 提交当前批次；`Fail` 策略遇错即停，其他策略记录警告后继续。
#[allow(clippy::too_many_arguments)]
async fn flush_batch(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    qualified: &str,
    policy: ConflictPolicy,
    cols: &[String],
    rows: &mut Vec<String>,
    first_line: u64,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let row_count = rows.len() as u64;
    let sql = build_insert_sql(config.driver, policy, qualified, cols, rows);
    rows.clear();
    match run_sql(svc, config, schema, &sql).await {
        Ok(affected) => {
            summary.items += affected;
            if matches!(policy, ConflictPolicy::Skip | ConflictPolicy::Merge) {
                summary.skipped += row_count.saturating_sub(affected);
            }
        }
        Err(error) => {
            if policy == ConflictPolicy::Fail {
                return Err(error);
            }
            summary.failed += row_count;
            summary.push_warning(format!(
                "自第 {first_line} 行起的 {row_count} 行批次执行失败：{}",
                error.message()
            ));
        }
    }
    reporter.snapshot.items_done = summary.items;
    reporter.emit();
    Ok(())
}

async fn run_sql(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    sql: &str,
) -> Result<u64> {
    let query = Query::new(sql.to_string()).with_schema(schema);
    let result = svc.execute(config, &query).await?;
    Ok(result.affected_rows)
}

/// 按表列顺序返回当前行的匹配列，并收集未知键。
fn present_columns(
    column_names: &[String],
    column_set: &HashSet<String>,
    object: &serde_json::Map<String, serde_json::Value>,
    unknown_keys: &mut BTreeSet<String>,
) -> Vec<String> {
    for key in object.keys() {
        if !column_set.contains(key) {
            unknown_keys.insert(key.clone());
        }
    }
    column_names
        .iter()
        .filter(|name| object.contains_key(*name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(json: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    #[test]
    fn present_columns_follow_table_order_and_collect_unknown() {
        let names: Vec<String> = ["id", "name", "note"].map(String::from).to_vec();
        let set: HashSet<String> = names.iter().cloned().collect();
        let mut unknown = BTreeSet::new();
        let row = object(r#"{"note": "x", "id": 1, "ghost": true}"#);
        let present = present_columns(&names, &set, &row, &mut unknown);
        assert_eq!(present, vec!["id".to_string(), "note".to_string()]);
        assert_eq!(unknown.iter().cloned().collect::<Vec<_>>(), vec!["ghost"]);
    }
}
