use super::*;

/// 在段开始时应用对象冲突策略。
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_policy(
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
                    // 保留已有对象，跳过 DDL；数据和索引段继续执行。
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

/// 加入完整 SQL 前先冲洗，确保批次不超限。
#[allow(clippy::too_many_arguments)]
pub(super) async fn queue_statement(
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

pub(super) fn sql_chunk_payload_limit(driver: DriverKind) -> usize {
    if driver == DriverKind::Mysql {
        CHUNK_FLUSH_BYTES.saturating_sub(MYSQL_IMPORT_PREFIX.len())
    } else {
        CHUNK_FLUSH_BYTES
    }
}

/// 执行段内累计的语句块。
pub(super) async fn flush_segment(
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

/// 在段结束时统计成功对象。
pub(super) fn finish_segment(
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
pub(super) fn is_use_statement(line: &str) -> bool {
    let lowered = line.trim().to_ascii_lowercase();
    lowered.starts_with("use ") && lowered.ends_with(';')
}

/// 合并策略的 INSERT 改写：MySQL 前缀 `INSERT IGNORE`；PG 尾附 `ON CONFLICT DO NOTHING`
/// （自家数据段一行一条语句，PG 要求行尾分号；不匹配的行返回 None 原样执行）
pub(super) fn merge_rewrite_line(line: &str, driver: DriverKind) -> Option<String> {
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

pub(super) async fn run_chunk_for(
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

pub(super) async fn run_chunk(
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
