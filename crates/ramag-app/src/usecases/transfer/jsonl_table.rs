//! 表级 JSONL 导入：每行一个 JSON 对象，键名匹配目标表列后分批 INSERT。
//! 与结果集 JSONL 导出配对，仅数据行、无 DDL；行内缺少的列不出现在
//! INSERT 列表（走库默认值 / 自增），未匹配表列的键忽略并汇总告警。
//! 策略语义：Skip=冲突行跳过、Overwrite=先清空表再导入、Fail=纯 INSERT 冲突即停；
//! Merge 在行级与 Skip 重合，调用方不提供该选项（防御性传入时按 Skip 处理）

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

use super::{Reporter, finish_summary, is_cancelled, read_line_bounded};
use crate::usecases::ConnectionService;

/// INSERT 批次的行数与字节阈值。
const BATCH_ROWS: usize = TRANSFER_BATCH_ITEMS;
const BATCH_BYTES: usize = TRANSFER_BATCH_BYTES;
/// 单行长度保护（异常长行直接拒绝，防脏文件撑爆内存）
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;
/// 未匹配键告警最多点名的键数
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
    let start = Instant::now();
    let (schema, table) = target;
    if config.production {
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if !matches!(config.driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err(DomainError::InvalidConfig(
            ".jsonl 表级导入仅支持 MySQL / PostgreSQL 连接".into(),
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
        // DELETE 而非 TRUNCATE：保持普通 DML 语义，外键约束冲突能正常暴露
        reporter.stage("清空表", format!("{schema}.{table}"));
        run_sql(svc, config, schema, &format!("DELETE FROM {qualified}")).await?;
    }
    reporter.stage("导入数据", format!("{schema}.{table}"));

    let mut line = String::new();
    let mut line_no: u64 = 0;
    let mut unknown_keys: BTreeSet<String> = BTreeSet::new();
    // 当前批：同列集的行攒成一条多行 INSERT；列集变化（异构行）先冲洗
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

/// 冲洗当前批：Fail 策略错误即停；其余策略计失败 + 告警后继续
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

/// 按表列顺序取出该行出现的列名；未匹配表列的键收集给调用方汇总告警
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

/// 渲染一行的 VALUES 元组；cols 即该行的 present 集，缺键不可达（防御填 NULL）
fn render_row(
    driver: DriverKind,
    cols: &[String],
    object: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut out = String::from("(");
    for (index, name) in cols.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match object.get(name) {
            Some(value) => out.push_str(&sql_literal(driver, value)),
            None => out.push_str("NULL"),
        }
    }
    out.push(')');
    out
}

/// JSON 值转 SQL 字面量；嵌套对象 / 数组按 JSON 文本入列（JSON 列可解析）
fn sql_literal(driver: DriverKind, value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(true) => "TRUE".to_string(),
        serde_json::Value::Bool(false) => "FALSE".to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => quote_string(driver, text),
        nested => quote_string(driver, &nested.to_string()),
    }
}

/// 字符串字面量：单引号翻倍；MySQL 默认反斜杠转义需再翻倍反斜杠，PG 标准串不需要
fn quote_string(driver: DriverKind, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' if driver == DriverKind::Mysql => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

fn quote_ident(driver: DriverKind, ident: &str) -> String {
    match driver {
        DriverKind::Mysql => format!("`{}`", ident.replace('`', "``")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

fn qualified_table(driver: DriverKind, schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        quote_ident(driver, schema),
        quote_ident(driver, table)
    )
}

/// 多行 INSERT：Skip/Merge 用引擎原生冲突跳过（MySQL IGNORE / PG DO NOTHING）
fn build_insert_sql(
    driver: DriverKind,
    policy: ConflictPolicy,
    qualified: &str,
    cols: &[String],
    rows: &[String],
) -> String {
    let dedupe = matches!(policy, ConflictPolicy::Skip | ConflictPolicy::Merge);
    let verb = if dedupe && driver == DriverKind::Mysql {
        "INSERT IGNORE INTO"
    } else {
        "INSERT INTO"
    };
    let suffix = if dedupe && driver == DriverKind::Postgres {
        "\nON CONFLICT DO NOTHING"
    } else {
        ""
    };
    let col_list = cols
        .iter()
        .map(|name| quote_ident(driver, name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{verb} {qualified} ({col_list}) VALUES\n{}{suffix}",
        rows.join(",\n")
    )
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
    fn ident_quoting_is_driver_specific() {
        assert_eq!(quote_ident(DriverKind::Mysql, "or`der"), "`or``der`");
        assert_eq!(
            quote_ident(DriverKind::Postgres, "or\"der"),
            "\"or\"\"der\""
        );
        assert_eq!(
            qualified_table(DriverKind::Mysql, "demo", "users"),
            "`demo`.`users`"
        );
    }

    #[test]
    fn literals_escape_per_driver() {
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::Value::Null),
            "NULL"
        );
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!(true)),
            "TRUE"
        );
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!(1.5)),
            "1.5"
        );
        // MySQL 反斜杠翻倍；PG 保持原样
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!("a'b\\c")),
            "'a''b\\\\c'"
        );
        assert_eq!(
            sql_literal(DriverKind::Postgres, &serde_json::json!("a'b\\c")),
            "'a''b\\c'"
        );
        // 嵌套结构按 JSON 文本入列
        assert_eq!(
            sql_literal(DriverKind::Postgres, &serde_json::json!({"k": 1})),
            "'{\"k\":1}'"
        );
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

    #[test]
    fn insert_sql_applies_policy_per_engine() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let rows = vec!["(1, 'a')".to_string(), "(2, 'b')".to_string()];
        let mysql_skip = build_insert_sql(
            DriverKind::Mysql,
            ConflictPolicy::Skip,
            "`d`.`t`",
            &cols,
            &rows,
        );
        assert!(mysql_skip.starts_with("INSERT IGNORE INTO `d`.`t` (`id`, `name`) VALUES"));
        let pg_skip = build_insert_sql(
            DriverKind::Postgres,
            ConflictPolicy::Skip,
            "\"d\".\"t\"",
            &cols,
            &rows,
        );
        assert!(pg_skip.ends_with("ON CONFLICT DO NOTHING"));
        let plain = build_insert_sql(
            DriverKind::Postgres,
            ConflictPolicy::Fail,
            "\"d\".\"t\"",
            &cols,
            &rows,
        );
        assert!(plain.starts_with("INSERT INTO"));
        assert!(!plain.contains("ON CONFLICT"));
    }

    #[test]
    fn render_row_serializes_in_column_order() {
        let row = object(r#"{"name": "张三", "id": 7}"#);
        let cols = vec!["id".to_string(), "name".to_string()];
        assert_eq!(render_row(DriverKind::Mysql, &cols, &row), "(7, '张三')");
    }
}
