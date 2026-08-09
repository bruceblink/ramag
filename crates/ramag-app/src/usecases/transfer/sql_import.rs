//! .sql 导入：流式读文件 → 按 ramag 段标记应用冲突策略 → 多语句分块执行。
//!
//! App 导出的文件按段（table/data/view/fk/…）组织；每块一次 execute（同连接多语句），
//! MySQL 块前缀 `SET FOREIGN_KEY_CHECKS=0` 消除建表 / 导数顺序问题。
//! 无标记的普通 .sql 走 generic 模式：顺序执行，错误按策略停止或计警告继续
mod execution;

use execution::*;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES, ProgressFn,
    Query, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, TransferSummary,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

use super::sql_catalog::parse_marker;
use super::{MYSQL_IMPORT_PREFIX, Reporter, finish_summary, is_cancelled, read_line_bounded};
use crate::usecases::ConnectionService;

const CHUNK_FLUSH_BYTES: usize = TRANSFER_BATCH_BYTES;
const CHUNK_FLUSH_STMTS: usize = TRANSFER_BATCH_ITEMS;
const MAX_LINE_BYTES: usize = TRANSFER_BATCH_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentKind {
    Header,
    Types,
    SequencesPre,
    Table,
    Data,
    Fk,
    Index,
    Sequences,
    View,
    Generic,
}

impl SegmentKind {
    fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "header" => Self::Header,
            "types" => Self::Types,
            "sequences-pre" => Self::SequencesPre,
            "table" => Self::Table,
            "data" => Self::Data,
            "fk" => Self::Fk,
            "index" => Self::Index,
            "sequences" => Self::Sequences,
            "view" => Self::View,
            _ => return None,
        })
    }

    /// 这些结构段允许因对象已存在而跳过错误。
    fn tolerates_errors(self) -> bool {
        matches!(
            self,
            Self::Types | Self::SequencesPre | Self::Fk | Self::Index | Self::Sequences
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::Header => "库结构",
            Self::Types => "枚举类型",
            Self::SequencesPre => "序列",
            Self::Table => "表结构",
            Self::Data => "表数据",
            Self::Fk => "外键",
            Self::Index => "索引",
            Self::Sequences => "序列值",
            Self::View => "视图",
            Self::Generic => "SQL 语句",
        }
    }
}

struct Segment {
    kind: SegmentKind,
    name: String,
    buffer: String,
    pending_statement: String,
    stmt_lines: usize,
    skip: bool,
    failed: bool,
}

/// 仅接收 Ramag 生成的单表文件，并恢复到文件所属的同名库。
///
/// SQL 文件包含可执行 DDL，不能把任意整库文件伪装成“导入表”；先校验范围头，
/// 再复用完整 SQL 导入器执行结构、约束、索引和数据恢复。
pub async fn import_sql_table(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    path: &Path,
    target_schema: &str,
    policy: ConflictPolicy,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let table = validate_table_export_header(path, config.driver, target_schema)?;
    import_sql(
        svc,
        config,
        path,
        policy,
        Some(target_schema),
        Some(&table),
        cancel,
        progress,
    )
    .await
}

fn validate_table_export_header(
    path: &Path,
    driver: DriverKind,
    target_schema: &str,
) -> Result<String> {
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    parse_table_export_header(BufReader::new(file), driver, target_schema)
}

fn parse_table_export_header(
    mut reader: impl BufRead,
    driver: DriverKind,
    target_schema: &str,
) -> Result<String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
    if line.trim_end() != "-- ramag table export v1" {
        return Err(DomainError::InvalidConfig(
            "请选择由 Ramag“导出此表”生成的单表 SQL 文件".into(),
        ));
    }

    let mut engine = None;
    let mut database = None;
    let mut table = None;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| DomainError::Storage(format!("读取导入文件失败：{error}")))?;
        if read == 0 || parse_marker(line.trim_end()).is_some() {
            break;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(DomainError::InvalidConfig(format!(
                "导入文件单行超过 {} MiB，疑似损坏",
                MAX_LINE_BYTES / 1024 / 1024
            )));
        }
        let trimmed = line.trim_end();
        if let Some(value) = trimmed.strip_prefix("-- engine: ") {
            engine = Some(value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("-- database: ") {
            database = Some(value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("-- table: ") {
            table = Some(value.trim().to_string());
        }
    }

    let expected_engine = match driver {
        DriverKind::Mysql => "mysql",
        DriverKind::Postgres => "postgres",
        _ => {
            return Err(DomainError::InvalidConfig(
                "单表 SQL 导入仅支持 MySQL / PostgreSQL 连接".into(),
            ));
        }
    };
    if engine.as_deref() != Some(expected_engine) {
        return Err(DomainError::InvalidConfig(format!(
            "单表文件引擎与当前 {expected_engine} 连接不匹配"
        )));
    }
    if database.as_deref() != Some(target_schema) {
        return Err(DomainError::InvalidConfig(format!(
            "单表文件所属库为「{}」，请选择对应库节点导入",
            database.as_deref().unwrap_or("未知")
        )));
    }
    let table =
        table.filter(|name| !name.is_empty() && name.len() <= MAX_CONNECTION_IDENTIFIER_BYTES);
    table.ok_or_else(|| DomainError::InvalidConfig("单表文件缺少有效的 table 头".into()))
}

impl Segment {
    fn new(kind: SegmentKind, name: &str) -> Self {
        Self {
            kind,
            name: name.to_string(),
            buffer: String::new(),
            pending_statement: String::new(),
            stmt_lines: 0,
            skip: false,
            failed: false,
        }
    }
}

pub async fn import_sql_database(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    path: &Path,
    policy: ConflictPolicy,
    fallback_schema: Option<&str>,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    import_sql(
        svc,
        config,
        path,
        policy,
        fallback_schema,
        None,
        cancel,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn import_sql(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    path: &Path,
    policy: ConflictPolicy,
    fallback_schema: Option<&str>,
    expected_table: Option<&str>,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    if config.production {
        warn!(
            operation = "sql_import",
            connection_id = %config.id,
            driver = ?config.driver,
            path = %path.display(),
            "read-only import blocked"
        );
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if !matches!(config.driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err(DomainError::InvalidConfig(
            ".sql 导入仅支持 MySQL / PostgreSQL 连接".into(),
        ));
    }
    info!(
        operation = "sql_import",
        connection_id = %config.id,
        driver = ?config.driver,
        target_schema = fallback_schema.unwrap_or("-"),
        target_table = expected_table.unwrap_or("*"),
        policy = ?policy,
        path = %path.display(),
        "transfer started"
    );
    let file = std::fs::File::open(path)
        .map_err(|error| DomainError::Storage(format!("打开导入文件失败：{error}")))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let mut summary = TransferSummary::default();
    let mut reporter = Reporter::new(progress);
    reporter.stage(
        "读取文件",
        path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
    );

    let mut header_engine: Option<String> = None;
    let mut header_database: Option<String> = None;
    // Ramag 文件使用文件头中的目标库，普通 SQL 使用调用方指定值。
    let mut schema: Option<String> = fallback_schema.map(str::to_string);
    let mut existing: HashMap<String, bool> = HashMap::new();
    let mut skipped_objects: HashSet<String> = HashSet::new();
    let mut failed_objects: HashSet<String> = HashSet::new();
    let mut expected_table_seen = false;

    let mut segment = Segment::new(SegmentKind::Generic, "");
    let mut line = String::new();
    loop {
        let read = read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES, "SQL 导入文件")?;
        let eof = read == 0;

        let trimmed = line.trim_end();
        let marker = if eof { None } else { parse_marker(trimmed) };
        let is_end_marker = trimmed == "-- ramag:end";
        if eof || marker.is_some() || is_end_marker {
            // 段切换前先提交上一段。
            if !segment.pending_statement.trim().is_empty() {
                let statement = std::mem::take(&mut segment.pending_statement);
                queue_statement(
                    svc,
                    config,
                    &schema,
                    &mut segment,
                    policy,
                    &mut summary,
                    statement,
                )
                .await?;
            }
            flush_segment(svc, config, &schema, &mut segment, policy, &mut summary).await?;
            finish_segment(&segment, &mut summary, &mut failed_objects, &mut reporter);
            if eof || is_end_marker {
                if eof {
                    break;
                }
                segment = Segment::new(SegmentKind::Generic, "");
                continue;
            }
            let (kind_text, name) = marker.unwrap_or(("", ""));
            let Some(kind) = SegmentKind::parse(kind_text) else {
                summary.push_warning(format!("未知段标记：{trimmed}"));
                segment = Segment::new(SegmentKind::Generic, "");
                continue;
            };
            if let Some(expected) = expected_table {
                match kind {
                    SegmentKind::Table => {
                        if name != expected || expected_table_seen {
                            return Err(DomainError::InvalidConfig(format!(
                                "单表文件包含范围外或重复表结构「{name}」"
                            )));
                        }
                        expected_table_seen = true;
                    }
                    SegmentKind::Data | SegmentKind::Index if name != expected => {
                        return Err(DomainError::InvalidConfig(format!(
                            "单表文件包含范围外对象「{name}」"
                        )));
                    }
                    SegmentKind::View => {
                        return Err(DomainError::InvalidConfig(
                            "单表文件不能包含视图定义".into(),
                        ));
                    }
                    _ => {}
                }
            }
            segment = Segment::new(kind, name);
            reporter.stage(format!("导入{}", kind.label()), name);

            // 进入内容前校验文件头并加载已有对象。
            if kind == SegmentKind::Header {
                let engine = header_engine.as_deref().unwrap_or("");
                let matches_engine = matches!(
                    (config.driver, engine),
                    (DriverKind::Mysql, "mysql") | (DriverKind::Postgres, "postgres")
                );
                if !matches_engine {
                    return Err(DomainError::InvalidConfig(format!(
                        "文件引擎（{engine}）与当前连接（{:?}）不匹配",
                        config.driver
                    )));
                }
                let Some(db) = header_database.clone() else {
                    return Err(DomainError::InvalidConfig(
                        "文件缺少 database 头，无法确定目标库".into(),
                    ));
                };
                let schema_exists = svc
                    .list_schemas(config)
                    .await?
                    .iter()
                    .any(|schema| schema.name == db);
                existing = if schema_exists {
                    svc.list_tables(config, &db)
                        .await?
                        .into_iter()
                        .map(|table| (table.name, table.is_view))
                        .collect()
                } else {
                    HashMap::new()
                };
                schema = Some(db);
            }
            apply_policy(
                svc,
                config,
                &schema,
                &mut segment,
                policy,
                &existing,
                &mut skipped_objects,
                &mut summary,
            )
            .await?;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("-- engine: ") {
            header_engine = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("-- database: ") {
            header_database = Some(rest.trim().to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.trim_start().starts_with("--") {
            // MySQL 预处理协议拒绝纯注释语句。
            continue;
        }
        if is_use_statement(trimmed) {
            // 目标库通过查询配置指定，预处理协议不执行文件中的 USE。
            continue;
        }

        if segment.skip || segment.failed {
            continue;
        }
        // 合并时将数据段 INSERT 改写为跳过冲突的形式。
        let rewritten = (policy == ConflictPolicy::Merge
            && (segment.kind == SegmentKind::Data
                || (segment.kind == SegmentKind::Generic && config.driver == DriverKind::Mysql)))
            .then(|| merge_rewrite_line(&line, config.driver))
            .flatten();
        match rewritten {
            Some(text) => {
                segment.pending_statement.push_str(&text);
                if !text.ends_with('\n') {
                    segment.pending_statement.push('\n');
                }
            }
            None => {
                segment.pending_statement.push_str(&line);
                if !line.ends_with('\n') {
                    segment.pending_statement.push('\n');
                }
            }
        }
        if segment.pending_statement.len() > sql_chunk_payload_limit(config.driver) {
            return Err(DomainError::InvalidConfig(format!(
                "单条 SQL 超过 {} MiB 安全上限，无法导入；请先拆分该语句",
                CHUNK_FLUSH_BYTES / 1024 / 1024
            )));
        }
        if trimmed.ends_with(';') {
            let statement = std::mem::take(&mut segment.pending_statement);
            queue_statement(
                svc,
                config,
                &schema,
                &mut segment,
                policy,
                &mut summary,
                statement,
            )
            .await?;
            reporter.snapshot.items_done = summary.items;
            reporter.emit();
        }
        if is_cancelled(cancel) {
            summary.cancelled = true;
            let summary = finish_summary(summary, start);
            info!(
                operation = "sql_import",
                connection_id = %config.id,
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

    if expected_table.is_some() && !expected_table_seen {
        return Err(DomainError::InvalidConfig(
            "单表文件缺少对应的表结构段".into(),
        ));
    }

    reporter.snapshot.items_done = summary.items;
    reporter.emit();
    let summary = finish_summary(summary, start);
    info!(
        operation = "sql_import",
        connection_id = %config.id,
        objects = summary.objects,
        items = summary.items,
        failed = summary.failed,
        cancelled = summary.cancelled,
        elapsed_ms = summary.elapsed_ms,
        "transfer finished"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests;
