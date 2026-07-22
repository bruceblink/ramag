//! .sql 导入：流式读文件 → 按 ramag 段标记应用冲突策略 → 多语句分块执行。
//!
//! App 导出的文件按段（table/data/view/fk/…）组织；每块一次 execute（同连接多语句），
//! MySQL 块前缀 `SET FOREIGN_KEY_CHECKS=0` 消除建表 / 导数顺序问题。
//! 无标记的普通 .sql 走 generic 模式：顺序执行，错误按策略停止或计警告继续

use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES, ProgressFn,
    Query, TRANSFER_BATCH_BYTES, TRANSFER_BATCH_ITEMS, TransferSummary,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};
use std::collections::{HashMap, HashSet};

use super::sql_catalog::parse_marker;
use super::{MYSQL_IMPORT_PREFIX, Reporter, finish_summary, is_cancelled, read_line_bounded};
use crate::usecases::ConnectionService;

/// 单块累计字节 / 语句数达到统一批次阈值即执行。
const CHUNK_FLUSH_BYTES: usize = TRANSFER_BATCH_BYTES;
const CHUNK_FLUSH_STMTS: usize = TRANSFER_BATCH_ITEMS;
/// 单行长度保护（自家文件单行 ≤ ~1 MiB；异常长行直接拒绝）
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

    /// 重复对象类错误只记警告的段（部分表被跳过时，这些段天然会撞已存在对象）
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
    /// 尚未遇到行尾分号的当前语句；不能在中间按字节截断。
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
        return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
    }
    if !matches!(config.driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err(DomainError::InvalidConfig(
            ".sql 导入仅支持 MySQL / PostgreSQL 连接".into(),
        ));
    }
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
    // 目标库（ramag 文件以文件头为准；generic 文件用调用方指定）
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
            // 段边界：先冲洗上一段，再切换
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

            // 进入首个内容段前，校验文件头并加载既有对象清单
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
                // 目标库可能尚不存在（header 段会创建），失败按空清单处理
                existing = svc
                    .list_tables(config, &db)
                    .await
                    .map(|tables| {
                        tables
                            .into_iter()
                            .map(|table| (table.name, table.is_view))
                            .collect()
                    })
                    .unwrap_or_default();
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

        // 文件头注释信息
        if let Some(rest) = trimmed.strip_prefix("-- engine: ") {
            header_engine = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("-- database: ") {
            header_database = Some(rest.trim().to_string());
            continue;
        }
        if trimmed.is_empty() || trimmed.trim_start().starts_with("--") {
            // 空行与 SQL 注释不进执行块（MySQL 预处理协议拒绝纯注释语句）
            continue;
        }
        if is_use_statement(trimmed) {
            // 目标库统一走 default_schema（COM_QUERY 简单协议）；
            // 文件内的 USE 行为 CLI 兼容保留，导入时跳过（预处理协议不支持 USE）
            continue;
        }

        if segment.skip || segment.failed {
            continue;
        }
        // 合并策略：数据段 INSERT 改写为条目级去重形态（generic 文件仅 MySQL 可安全按行改写）
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
            return Ok(finish_summary(summary, start));
        }
    }

    if expected_table.is_some() && !expected_table_seen {
        return Err(DomainError::InvalidConfig(
            "单表文件缺少对应的表结构段".into(),
        ));
    }

    reporter.snapshot.items_done = summary.items;
    reporter.emit();
    Ok(finish_summary(summary, start))
}

/// 段开始时应用冲突策略（存在性检查 + Overwrite 的 DROP）
#[allow(clippy::too_many_arguments)]
async fn apply_policy(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &Option<String>,
    segment: &mut Segment,
    policy: ConflictPolicy,
    existing: &HashMap<String, bool>,
    skipped_objects: &mut HashSet<String>,
    summary: &mut TransferSummary,
) -> Result<()> {
    match segment.kind {
        SegmentKind::Table | SegmentKind::View => {
            let is_view_segment = segment.kind == SegmentKind::View;
            let exists = existing
                .get(&segment.name)
                .is_some_and(|is_view| *is_view == is_view_segment);
            if !exists {
                return Ok(());
            }
            match policy {
                ConflictPolicy::Skip => {
                    segment.skip = true;
                    skipped_objects.insert(segment.name.clone());
                    summary.skipped += 1;
                }
                ConflictPolicy::Merge => {
                    // 保留已存在对象：跳过其 DDL 段本身；数据 / 索引段照常执行，
                    // 数据段的 INSERT 已被改写为条目级去重形态
                    segment.skip = true;
                }
                ConflictPolicy::Fail => {
                    return Err(DomainError::QueryFailed(format!(
                        "{}「{}」已存在（冲突策略：报错停止）",
                        if is_view_segment { "视图" } else { "表" },
                        segment.name
                    )));
                }
                ConflictPolicy::Overwrite => {
                    let Some(db) = schema.as_deref() else {
                        return Ok(());
                    };
                    let qualified = format!(
                        "{}.{}",
                        config.driver.quote_identifier(db),
                        config.driver.quote_identifier(&segment.name)
                    );
                    let drop_sql = match (config.driver, is_view_segment) {
                        (DriverKind::Mysql, false) => {
                            format!("DROP TABLE IF EXISTS {qualified};")
                        }
                        (DriverKind::Mysql, true) => format!("DROP VIEW IF EXISTS {qualified};"),
                        (_, false) => format!("DROP TABLE IF EXISTS {qualified} CASCADE;"),
                        (_, true) => format!("DROP VIEW IF EXISTS {qualified} CASCADE;"),
                    };
                    if let Err(error) = run_chunk(svc, config, schema, &drop_sql).await {
                        segment.failed = true;
                        summary.failed += 1;
                        summary.push_warning(format!(
                            "覆盖删除 {} 失败：{}",
                            segment.name,
                            error.message()
                        ));
                    }
                }
            }
        }
        SegmentKind::Data | SegmentKind::Index if skipped_objects.contains(&segment.name) => {
            segment.skip = true;
        }
        _ => {}
    }
    Ok(())
}

/// 把一条完整 SQL 加入待执行批次；加入前先冲洗，保证实际请求不越过 32 MiB。
#[allow(clippy::too_many_arguments)]
async fn queue_statement(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &Option<String>,
    segment: &mut Segment,
    policy: ConflictPolicy,
    summary: &mut TransferSummary,
    statement: String,
) -> Result<()> {
    if statement.trim().is_empty() || segment.skip || segment.failed {
        return Ok(());
    }
    let payload_limit = sql_chunk_payload_limit(config.driver);
    if statement.len() > payload_limit {
        return Err(DomainError::InvalidConfig(format!(
            "单条 SQL 超过 {} MiB 安全上限，无法导入；请先拆分该语句",
            CHUNK_FLUSH_BYTES / 1024 / 1024
        )));
    }
    let prospective = segment.buffer.len().saturating_add(statement.len());
    if !segment.buffer.is_empty()
        && (segment.stmt_lines >= CHUNK_FLUSH_STMTS || prospective > payload_limit)
    {
        flush_segment(svc, config, schema, segment, policy, summary).await?;
    }
    if segment.failed {
        return Ok(());
    }
    segment.buffer.push_str(&statement);
    segment.stmt_lines = segment.stmt_lines.saturating_add(1);
    if segment.stmt_lines >= CHUNK_FLUSH_STMTS || segment.buffer.len() >= payload_limit {
        flush_segment(svc, config, schema, segment, policy, summary).await?;
    }
    Ok(())
}

fn sql_chunk_payload_limit(driver: DriverKind) -> usize {
    if driver == DriverKind::Mysql {
        CHUNK_FLUSH_BYTES.saturating_sub(MYSQL_IMPORT_PREFIX.len())
    } else {
        CHUNK_FLUSH_BYTES
    }
}

/// 执行段内累计的语句块
async fn flush_segment(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &Option<String>,
    segment: &mut Segment,
    policy: ConflictPolicy,
    summary: &mut TransferSummary,
) -> Result<()> {
    if segment.skip || segment.failed || segment.buffer.trim().is_empty() {
        segment.buffer.clear();
        segment.stmt_lines = 0;
        return Ok(());
    }
    let sql = std::mem::take(&mut segment.buffer);
    segment.stmt_lines = 0;
    match run_chunk_for(svc, config, schema, segment.kind, &sql).await {
        Ok(affected) => {
            summary.items += affected;
        }
        Err(error) => {
            if policy == ConflictPolicy::Fail {
                return Err(DomainError::QueryFailed(format!(
                    "导入{}「{}」失败：{}",
                    segment.kind.label(),
                    segment.name,
                    error.message()
                )));
            }
            if segment.kind.tolerates_errors() {
                summary.push_warning(format!(
                    "{}语句被跳过：{}",
                    segment.kind.label(),
                    error.message()
                ));
            } else {
                segment.failed = true;
                summary.failed += 1;
                summary.push_warning(format!(
                    "导入{}「{}」失败：{}",
                    segment.kind.label(),
                    segment.name,
                    error.message()
                ));
            }
        }
    }
    Ok(())
}

/// 段收尾：计数成功对象
fn finish_segment(
    segment: &Segment,
    summary: &mut TransferSummary,
    failed_objects: &mut HashSet<String>,
    reporter: &mut Reporter<'_>,
) {
    if segment.failed {
        failed_objects.insert(segment.name.clone());
        return;
    }
    if segment.skip {
        return;
    }
    if matches!(segment.kind, SegmentKind::Table | SegmentKind::View) {
        summary.objects += 1;
        reporter.snapshot.objects_done += 1;
    }
}

/// 单行 `USE xxx;` 语句判定（自家导出文件的 USE 独占一行）
fn is_use_statement(line: &str) -> bool {
    let lowered = line.trim().to_ascii_lowercase();
    lowered.starts_with("use ") && lowered.ends_with(';')
}

/// 合并策略的 INSERT 改写：MySQL 前缀 `INSERT IGNORE`；PG 尾附 `ON CONFLICT DO NOTHING`
/// （自家数据段一行一条语句，PG 要求行尾分号；不匹配的行返回 None 原样执行）
fn merge_rewrite_line(line: &str, driver: DriverKind) -> Option<String> {
    if !line.trim_start().starts_with("INSERT INTO ") {
        return None;
    }
    match driver {
        DriverKind::Mysql => Some(line.replacen("INSERT INTO ", "INSERT IGNORE INTO ", 1)),
        DriverKind::Postgres => {
            let body = line.trim_end().strip_suffix(';')?;
            Some(format!("{body} ON CONFLICT DO NOTHING;\n"))
        }
        _ => None,
    }
}

async fn run_chunk_for(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &Option<String>,
    kind: SegmentKind,
    sql: &str,
) -> Result<u64> {
    // header 段自带 CREATE DATABASE / USE，且目标库可能还不存在，不能预切库
    let schema_for_call = if kind == SegmentKind::Header {
        &None
    } else {
        schema
    };
    run_chunk(svc, config, schema_for_call, sql).await
}

async fn run_chunk(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &Option<String>,
    sql: &str,
) -> Result<u64> {
    // MySQL 每块关闭 FK 检查：建表顺序 / 数据顺序 / 覆盖删除都无需拓扑排序
    let effective = if config.driver == DriverKind::Mysql {
        format!("{MYSQL_IMPORT_PREFIX}{sql}")
    } else {
        sql.to_string()
    };
    let mut query = Query::new(effective);
    if let Some(db) = schema.as_deref() {
        query = query.with_schema(db);
    }
    let result = svc.execute(config, &query).await?;
    Ok(result.affected_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_kinds_parse_and_classify() {
        assert_eq!(SegmentKind::parse("table"), Some(SegmentKind::Table));
        assert_eq!(
            SegmentKind::parse("sequences-pre"),
            Some(SegmentKind::SequencesPre)
        );
        assert_eq!(SegmentKind::parse("bogus"), None);
        assert!(SegmentKind::Fk.tolerates_errors());
        assert!(!SegmentKind::Data.tolerates_errors());
    }

    #[test]
    fn merge_rewrites_insert_statements_per_dialect() {
        let line = "INSERT INTO `t` (`a`) VALUES (1), (2);\n";
        assert_eq!(
            merge_rewrite_line(line, DriverKind::Mysql).as_deref(),
            Some("INSERT IGNORE INTO `t` (`a`) VALUES (1), (2);\n")
        );
        let pg = "INSERT INTO \"s\".\"t\" (\"a\") OVERRIDING SYSTEM VALUE VALUES (1);\n";
        assert_eq!(
            merge_rewrite_line(pg, DriverKind::Postgres).as_deref(),
            Some(
                "INSERT INTO \"s\".\"t\" (\"a\") OVERRIDING SYSTEM VALUE VALUES (1) ON CONFLICT DO NOTHING;\n"
            )
        );
        // 非 INSERT 行不改写
        assert_eq!(
            merge_rewrite_line("CREATE TABLE t (a int);\n", DriverKind::Mysql),
            None
        );
        // PG 行尾无分号（异常形态）不强行改写
        assert_eq!(
            merge_rewrite_line("INSERT INTO t VALUES (1)", DriverKind::Postgres),
            None
        );
    }

    #[test]
    fn use_lines_are_recognized() {
        assert!(is_use_statement("USE `shop`;"));
        assert!(is_use_statement("  use x;  "));
        assert!(!is_use_statement("USE `shop`"));
        assert!(!is_use_statement("SELECT usedata FROM t;"));
        assert!(!is_use_statement("-- use note;"));
    }

    #[test]
    fn table_import_header_requires_single_table_export_for_current_database() {
        let valid = "-- ramag table export v1\n\
                     -- engine: postgres\n\
                     -- database: public\n\
                     -- table: users\n\
                     -- ramag:begin header\n";
        assert_eq!(
            parse_table_export_header(std::io::Cursor::new(valid), DriverKind::Postgres, "public")
                .unwrap(),
            "users"
        );
        assert!(
            parse_table_export_header(std::io::Cursor::new(valid), DriverKind::Postgres, "archive")
                .is_err()
        );

        let database_export = valid.replacen("table export", "database export", 1);
        assert!(
            parse_table_export_header(
                std::io::Cursor::new(database_export),
                DriverKind::Postgres,
                "public"
            )
            .is_err()
        );
    }
}
