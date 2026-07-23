//! MySQL / PostgreSQL 按库或单表导出为 .sql。
//!
//! 结构顺序（导入端零拓扑排序）：
//! MySQL：header(FK_CHECKS=0) → 每表 [DDL(SHOW CREATE，FK 内联) → 数据] → 视图；
//! PG：header → 枚举类型 → serial 序列预建 → 每表 [DDL(无 FK) + OWNED BY + 注释 → 数据]
//!     → FK ALTER → 索引 → setval → 视图。
//! 数据用主键 keyset 分页（无主键退化 OFFSET 并告警）；非快照一致（文件头注明）

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use ramag_domain::entities::{
    Column, ConnectionConfig, DriverKind, ProgressFn, Query, TRANSFER_BATCH_BYTES,
    TRANSFER_BATCH_ITEMS, Table, TransferSummary, Value, build_ddl_query,
};
use ramag_domain::error::{DomainError, Result};

use super::sql_catalog::{
    PgSequenceInfo, begin_marker, first_column_strings, generated_columns_query,
    parse_pg_sequences, parse_show_create, pg_comments_query, pg_enum_types_query,
    pg_foreign_keys_query, pg_indexes_query, pg_sequences_query, pg_table_create_query,
    pg_table_enum_types_query, pg_table_foreign_keys_query, transfer_literal,
};
use super::{
    ExportSink, MYSQL_IMPORT_PREFIX, Reporter, finish_summary, is_cancelled, with_export_sink,
};
use crate::usecases::ConnectionService;

/// 导出读取与 INSERT 生成共用批次上限。
const PAGE_ROWS: u32 = TRANSFER_BATCH_ITEMS as u32;
const INSERT_FLUSH_BYTES: usize = TRANSFER_BATCH_BYTES;
const INSERT_MAX_ROWS: usize = TRANSFER_BATCH_ITEMS;

fn sql_transfer_payload_limit(driver: DriverKind) -> usize {
    if driver == DriverKind::Mysql {
        TRANSFER_BATCH_BYTES.saturating_sub(MYSQL_IMPORT_PREFIX.len())
    } else {
        TRANSFER_BATCH_BYTES
    }
}

fn write_sql_statement(
    sink: &mut ExportSink,
    driver: DriverKind,
    statement: &str,
    label: &str,
) -> Result<()> {
    if statement.len().saturating_add(1) > sql_transfer_payload_limit(driver) {
        return Err(DomainError::InvalidConfig(format!(
            "{label}的单条 SQL 超过 {} MiB，无法按安全批次导出",
            TRANSFER_BATCH_BYTES / 1024 / 1024
        )));
    }
    sink.write_str(statement)?;
    sink.write_str("\n")
}

pub async fn export_sql_database(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    export_sql(svc, config, schema, None, path, cancel, progress).await
}

/// 导出单表结构与全部数据；文件沿用库级 SQL 协议，可直接走现有 SQL 导入恢复。
pub async fn export_sql_table(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    target: (&str, &str),
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let (schema, table) = target;
    export_sql(svc, config, schema, Some(table), path, cancel, progress).await
}

async fn export_sql(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    target_table: Option<&str>,
    path: &Path,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
) -> Result<TransferSummary> {
    let start = Instant::now();
    let driver = config.driver;
    if !matches!(driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err(DomainError::InvalidConfig(
            "按库导出仅支持 MySQL / PostgreSQL 连接".into(),
        ));
    }
    let all = svc.list_tables(config, schema).await?;
    let (tables, views): (Vec<Table>, Vec<Table>) = match target_table {
        Some(name) => {
            let table = all
                .into_iter()
                .find(|item| item.name == name)
                .ok_or_else(|| DomainError::NotFound(format!("表 {schema}.{name} 不存在")))?;
            if table.is_view {
                return Err(DomainError::InvalidConfig(
                    "视图不支持表级结构与数据导出，请使用库级导出".into(),
                ));
            }
            (vec![table], Vec::new())
        }
        None => all.into_iter().partition(|table| !table.is_view),
    };

    with_export_sink(path, |mut sink| async move {
        let mut summary = TransferSummary::default();
        let mut reporter = Reporter::new(progress);
        reporter.snapshot.objects_total = Some((tables.len() + views.len()) as u64);
        reporter.stage(
            "读取结构",
            target_table
                .map(|table| format!("{schema}.{table}"))
                .unwrap_or_else(|| schema.to_string()),
        );

        write_header(svc, config, schema, target_table, driver, &mut sink).await?;

        // PG：枚举类型 + serial 序列预建（DEFAULT nextval 引用它们，必须先建）
        let mut sequences: Vec<(String, PgSequenceInfo)> = Vec::new();
        if driver == DriverKind::Postgres {
            let enum_query = target_table
                .map(|table| pg_table_enum_types_query(schema, table))
                .unwrap_or_else(|| pg_enum_types_query(schema));
            let enums = run_first_column(svc, config, enum_query).await?;
            if !enums.is_empty() {
                sink.write_str(&begin_marker("types", ""))?;
                for stmt in &enums {
                    write_sql_statement(&mut sink, driver, stmt, "PostgreSQL 枚举类型")?;
                }
            }
            for table in &tables {
                let result = svc
                    .execute(config, &Query::new(pg_sequences_query(schema, &table.name)))
                    .await?;
                sequences.push((table.name.clone(), parse_pg_sequences(&result)));
            }
            let creates: Vec<&String> = sequences
                .iter()
                .flat_map(|(_, info)| info.create_stmts.iter())
                .collect();
            if !creates.is_empty() {
                sink.write_str(&begin_marker("sequences-pre", ""))?;
                for stmt in creates {
                    write_sql_statement(&mut sink, driver, stmt, "PostgreSQL 序列")?;
                }
            }
        }

        for table in &tables {
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            reporter.stage("导出表结构", &table.name);
            write_table_ddl(svc, config, schema, table, &sequences, &mut sink).await?;
            export_table_data(
                svc,
                config,
                schema,
                table,
                &sequences,
                cancel,
                &mut sink,
                &mut summary,
                &mut reporter,
            )
            .await?;
            if summary.cancelled {
                return Ok(finish_summary(summary, start));
            }
            summary.objects += 1;
            reporter.snapshot.objects_done += 1;
            reporter.snapshot.bytes = sink.bytes_written();
            reporter.emit();
        }

        if driver == DriverKind::Postgres {
            let fk_query = target_table
                .map(|table| pg_table_foreign_keys_query(schema, table))
                .unwrap_or_else(|| pg_foreign_keys_query(schema));
            let fk = run_first_column(svc, config, fk_query).await?;
            if !fk.is_empty() {
                sink.write_str(&begin_marker("fk", ""))?;
                for stmt in &fk {
                    write_sql_statement(&mut sink, driver, stmt, "PostgreSQL 外键")?;
                }
            }
            for table in &tables {
                let stmts =
                    run_first_column(svc, config, pg_indexes_query(schema, &table.name)).await?;
                if !stmts.is_empty() {
                    sink.write_str(&begin_marker("index", &table.name))?;
                    for stmt in &stmts {
                        write_sql_statement(&mut sink, driver, stmt, "PostgreSQL 索引")?;
                    }
                }
            }
            let setvals: Vec<&String> = sequences
                .iter()
                .flat_map(|(_, info)| info.setval_stmts.iter())
                .collect();
            if !setvals.is_empty() {
                reporter.stage("同步序列", schema);
                sink.write_str(&begin_marker("sequences", ""))?;
                for stmt in setvals {
                    write_sql_statement(&mut sink, driver, stmt, "PostgreSQL 序列值")?;
                }
            }
        }

        for view in &views {
            if is_cancelled(cancel) {
                summary.cancelled = true;
                return Ok(finish_summary(summary, start));
            }
            reporter.stage("导出视图", &view.name);
            match view_ddl(svc, config, schema, &view.name, driver).await {
                Ok(ddl) => {
                    sink.write_str(&begin_marker("view", &view.name))?;
                    write_sql_statement(&mut sink, driver, &ddl, "视图结构")?;
                    summary.objects += 1;
                }
                Err(error) => {
                    summary.failed += 1;
                    summary.push_warning(format!(
                        "视图 {} 导出失败：{}",
                        view.name,
                        error.message()
                    ));
                }
            }
            reporter.snapshot.objects_done += 1;
        }

        sink.write_str("-- ramag:end\n")?;
        summary.bytes = sink.bytes_written();
        reporter.snapshot.bytes = summary.bytes;
        reporter.emit();
        sink.finish()?;
        Ok(finish_summary(summary, start))
    })
    .await
}

async fn write_header(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    target_table: Option<&str>,
    driver: DriverKind,
    sink: &mut ExportSink,
) -> Result<()> {
    let engine = match driver {
        DriverKind::Mysql => "mysql",
        _ => "postgres",
    };
    let kind = if target_table.is_some() {
        "table"
    } else {
        "database"
    };
    sink.write_str(&format!(
        "-- ramag {kind} export v1\n-- engine: {engine}\n-- database: {schema}\n"
    ))?;
    if let Some(table) = target_table {
        sink.write_str(&format!("-- table: {table}\n"))?;
    }
    sink.write_str(&format!(
        "-- exported_at: {}\n\
         -- 说明：不含触发器 / 存储过程 / 事件 / 权限；导出为非快照一致\n",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    ))?;
    sink.write_str(&begin_marker("header", ""))?;
    match driver {
        DriverKind::Mysql => {
            // 建库带上源库字符集 / 排序规则（取不到就用服务端默认）
            let charset_clause = svc
                .list_schemas(config)
                .await
                .ok()
                .and_then(|schemas| schemas.into_iter().find(|s| s.name == schema))
                .map(|s| {
                    let mut clause = String::new();
                    if let Some(charset) = s.charset {
                        clause.push_str(&format!(" DEFAULT CHARACTER SET {charset}"));
                    }
                    if let Some(collation) = s.collation {
                        clause.push_str(&format!(" COLLATE {collation}"));
                    }
                    clause
                })
                .unwrap_or_default();
            let quoted = driver.quote_identifier(schema);
            sink.write_str("SET NAMES utf8mb4;\n")?;
            sink.write_str(MYSQL_IMPORT_PREFIX)?;
            sink.write_str(&format!(
                "CREATE DATABASE IF NOT EXISTS {quoted}{charset_clause};\nUSE {quoted};\n"
            ))?;
        }
        _ => {
            sink.write_str(&format!(
                "CREATE SCHEMA IF NOT EXISTS {};\n",
                driver.quote_identifier(schema)
            ))?;
        }
    }
    Ok(())
}

async fn write_table_ddl(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    table: &Table,
    sequences: &[(String, PgSequenceInfo)],
    sink: &mut ExportSink,
) -> Result<()> {
    sink.write_str(&begin_marker("table", &table.name))?;
    match config.driver {
        DriverKind::Mysql => {
            let sql = build_ddl_query(DriverKind::Mysql, schema, &table.name, false);
            let result = svc.execute(config, &Query::new(sql)).await?;
            let statement = format!("{};", parse_show_create(&result)?);
            write_sql_statement(sink, config.driver, &statement, "MySQL 表结构")?;
        }
        _ => {
            // Overwrite 导入会在 table 段开始前删除旧表；serial 的 OWNED 序列也会被级联删除。
            // 因此在同一 table 段内再保证一次序列存在，避免随后 DEFAULT nextval 建表失败。
            if let Some((_, info)) = sequences.iter().find(|(name, _)| name == &table.name) {
                for stmt in &info.create_stmts {
                    sink.write_str(stmt)?;
                    sink.write_str("\n")?;
                }
            }
            let create = run_first_column(svc, config, pg_table_create_query(schema, &table.name))
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    DomainError::QueryFailed(format!("表 {} 结构查询无结果", table.name))
                })?;
            write_sql_statement(sink, config.driver, &create, "PostgreSQL 表结构")?;
            if let Some((_, info)) = sequences.iter().find(|(name, _)| name == &table.name) {
                for stmt in &info.owned_stmts {
                    write_sql_statement(sink, config.driver, stmt, "PostgreSQL 序列归属")?;
                }
            }
            for stmt in
                run_first_column(svc, config, pg_comments_query(schema, &table.name)).await?
            {
                write_sql_statement(sink, config.driver, &stmt, "PostgreSQL 注释")?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn export_table_data(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    table: &Table,
    sequences: &[(String, PgSequenceInfo)],
    cancel: &AtomicBool,
    sink: &mut ExportSink,
    summary: &mut TransferSummary,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    let driver = config.driver;
    let columns = svc.list_columns(config, schema, &table.name).await?;
    if columns.is_empty() {
        summary.push_warning(format!("表 {} 无列信息，跳过数据导出", table.name));
        return Ok(());
    }
    let generated = run_first_column(
        svc,
        config,
        generated_columns_query(driver, schema, &table.name),
    )
    .await
    .unwrap_or_default();
    let export_cols: Vec<&Column> = columns
        .iter()
        .filter(|c| !generated.contains(&c.name))
        .collect();
    if export_cols.is_empty() {
        summary.push_warning(format!("表 {} 全部列为生成列，跳过数据导出", table.name));
        return Ok(());
    }
    let pk: Vec<&Column> = export_cols
        .iter()
        .copied()
        .filter(|c| c.is_primary_key)
        .collect();
    let use_keyset = !pk.is_empty();
    if !use_keyset {
        summary.push_warning(format!(
            "表 {} 无主键，使用 OFFSET 分页导出（大表较慢，且并发写入下可能重复/遗漏）",
            table.name
        ));
    }
    // PG 有 GENERATED ALWAYS AS IDENTITY 列时，显式插入需 OVERRIDING SYSTEM VALUE
    let overriding = sequences
        .iter()
        .find(|(name, _)| name == &table.name)
        .is_some_and(|(_, info)| info.has_identity_always);

    let qualified = qualified_name(driver, schema, &table.name);
    let col_list = export_cols
        .iter()
        .map(|c| driver.quote_identifier(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let order_by = pk
        .iter()
        .map(|c| driver.quote_identifier(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_target = match driver {
        // MySQL 文件内不带库前缀：导入端 USE / default_schema 决定目标库
        DriverKind::Mysql => driver.quote_identifier(&table.name),
        _ => qualified.clone(),
    };
    let insert_prefix = format!(
        "INSERT INTO {insert_target} ({col_list}){} VALUES ",
        if overriding {
            " OVERRIDING SYSTEM VALUE"
        } else {
            ""
        }
    );
    let pk_indices: Vec<usize> = pk
        .iter()
        .map(|p| {
            export_cols
                .iter()
                .position(|c| c.name == p.name)
                .unwrap_or_default()
        })
        .collect();

    sink.write_str(&begin_marker("data", &table.name))?;
    reporter.stage("导出数据", &table.name);

    let mut last_key: Option<Vec<Value>> = None;
    let mut offset: u64 = 0;
    let mut insert_buf = String::with_capacity(INSERT_FLUSH_BYTES + 4096);
    let mut buffered_rows = 0usize;
    // 预留 MySQL 导入时追加的外键检查前缀。
    let insert_payload_limit = sql_transfer_payload_limit(driver);
    loop {
        if is_cancelled(cancel) {
            summary.cancelled = true;
            return Ok(());
        }
        let select = build_page_select(
            driver, &qualified, &col_list, &pk, &order_by, &last_key, offset,
        );
        let result = svc
            .execute(
                config,
                &Query::new(select).with_result_byte_limit(TRANSFER_BATCH_BYTES),
            )
            .await?;
        if result.rows.is_empty() {
            if result.truncated {
                return Err(DomainError::InvalidConfig(format!(
                    "表 {} 的单行或列元数据超过 {} MiB，无法按安全批次导出",
                    table.name,
                    TRANSFER_BATCH_BYTES / 1024 / 1024
                )));
            }
            break;
        }
        let page_len = result.rows.len() as u64;
        for row in &result.rows {
            let mut row_sql = String::from("(");
            for (index, value) in row.values.iter().enumerate() {
                if index > 0 {
                    row_sql.push_str(", ");
                }
                row_sql.push_str(&transfer_literal(value, driver));
            }
            row_sql.push(')');

            let single_statement_bytes = insert_prefix
                .len()
                .saturating_add(row_sql.len())
                .saturating_add(2);
            if single_statement_bytes > insert_payload_limit {
                return Err(DomainError::InvalidConfig(format!(
                    "表 {} 的单行 INSERT 超过 {} MiB，无法按安全批次导出",
                    table.name,
                    INSERT_FLUSH_BYTES / 1024 / 1024
                )));
            }
            let separator_bytes = usize::from(buffered_rows > 0) * 2;
            let prospective_bytes = insert_buf
                .len()
                .saturating_add(separator_bytes)
                .saturating_add(row_sql.len())
                .saturating_add(2);
            if buffered_rows > 0
                && (buffered_rows >= INSERT_MAX_ROWS || prospective_bytes > insert_payload_limit)
            {
                insert_buf.push_str(";\n");
                sink.write_str(&insert_buf)?;
                insert_buf.clear();
                buffered_rows = 0;
            }
            if buffered_rows == 0 {
                insert_buf.push_str(&insert_prefix);
            } else {
                insert_buf.push_str(", ");
            }
            insert_buf.push_str(&row_sql);
            buffered_rows += 1;
            if buffered_rows >= INSERT_MAX_ROWS {
                insert_buf.push_str(";\n");
                sink.write_str(&insert_buf)?;
                insert_buf.clear();
                buffered_rows = 0;
            }
        }
        summary.items += page_len;
        reporter.snapshot.items_done += page_len;
        reporter.snapshot.bytes = sink.bytes_written();
        reporter.emit();

        if use_keyset {
            // 按实际返回的最后一行推进；即使 driver 因内存上限截断分页也不丢行
            if let Some(last_row) = result.rows.last() {
                last_key = Some(
                    pk_indices
                        .iter()
                        .map(|&i| last_row.values.get(i).cloned().unwrap_or(Value::Null))
                        .collect(),
                );
            }
        } else {
            offset += page_len;
        }
    }
    if buffered_rows > 0 {
        insert_buf.push_str(";\n");
        sink.write_str(&insert_buf)?;
    }
    Ok(())
}

fn build_page_select(
    driver: DriverKind,
    qualified: &str,
    col_list: &str,
    pk: &[&Column],
    order_by: &str,
    last_key: &Option<Vec<Value>>,
    offset: u64,
) -> String {
    if pk.is_empty() {
        return format!("SELECT {col_list} FROM {qualified} LIMIT {PAGE_ROWS} OFFSET {offset};");
    }
    let where_clause = match last_key {
        None => String::new(),
        Some(values) => {
            let literals = values
                .iter()
                .map(|v| transfer_literal(v, driver))
                .collect::<Vec<_>>()
                .join(", ");
            if pk.len() == 1 {
                format!(
                    " WHERE {} > {literals}",
                    driver.quote_identifier(&pk[0].name)
                )
            } else {
                let cols = pk
                    .iter()
                    .map(|c| driver.quote_identifier(&c.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" WHERE ({cols}) > ({literals})")
            }
        }
    };
    format!(
        "SELECT {col_list} FROM {qualified}{where_clause} ORDER BY {order_by} LIMIT {PAGE_ROWS};"
    )
}

async fn view_ddl(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    view: &str,
    driver: DriverKind,
) -> Result<String> {
    let sql = build_ddl_query(driver, schema, view, true);
    let result = svc.execute(config, &Query::new(sql)).await?;
    match driver {
        DriverKind::Mysql => {
            // DEFINER 依赖导出侧账号，导入到别的实例会因用户不存在而失败，剥掉
            Ok(format!(
                "{};",
                strip_mysql_definer(&parse_show_create(&result)?)
            ))
        }
        _ => {
            let definition = first_column_strings(&result)
                .into_iter()
                .next()
                .ok_or_else(|| DomainError::QueryFailed(format!("视图 {view} 定义查询无结果")))?;
            let qualified = qualified_name(driver, schema, view);
            let body = definition.trim_end().trim_end_matches(';');
            Ok(format!("CREATE VIEW {qualified} AS\n{body};"))
        }
    }
}

fn qualified_name(driver: DriverKind, schema: &str, name: &str) -> String {
    format!(
        "{}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(name)
    )
}

/// 去掉 MySQL DDL 里的 DEFINER=`user`@`host` 子句
fn strip_mysql_definer(ddl: &str) -> String {
    let Some(start) = ddl.find(" DEFINER=") else {
        return ddl.to_string();
    };
    let rest = &ddl[start + " DEFINER=".len()..];
    // DEFINER=`u`@`h` / DEFINER=u@h，跳过两个可能带反引号的段
    let mut chars = rest.char_indices().peekable();
    let mut segments = 0;
    let mut in_quote = false;
    let mut end = rest.len();
    for (i, ch) in chars.by_ref() {
        match ch {
            '`' => in_quote = !in_quote,
            '@' if !in_quote && segments == 0 => segments = 1,
            ' ' if !in_quote => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    format!("{}{}", &ddl[..start], &rest[end..])
}

async fn run_first_column(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    sql: String,
) -> Result<Vec<String>> {
    let result = svc.execute(config, &Query::new(sql)).await?;
    Ok(first_column_strings(&result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{ColumnKind, ColumnType};

    fn pk_col(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: ColumnType {
                kind: ColumnKind::Integer,
                raw_type: "int".into(),
            },
            nullable: false,
            default_value: None,
            is_primary_key: true,
            comment: None,
        }
    }

    #[test]
    fn keyset_select_uses_row_constructor_only_for_composite_pk() {
        let a = pk_col("a");
        let b = pk_col("b");
        let single = vec![&a];
        let composite = vec![&a, &b];
        let first = build_page_select(
            DriverKind::Mysql,
            "`d`.`t`",
            "`a`, `b`",
            &single,
            "`a`",
            &None,
            0,
        );
        assert!(first.contains("ORDER BY `a` LIMIT"));
        assert!(!first.contains("WHERE"));

        let next = build_page_select(
            DriverKind::Mysql,
            "`d`.`t`",
            "`a`, `b`",
            &single,
            "`a`",
            &Some(vec![Value::Int(7)]),
            0,
        );
        assert!(next.contains("WHERE `a` > 7"));

        let composite_next = build_page_select(
            DriverKind::Postgres,
            "\"s\".\"t\"",
            "\"a\", \"b\"",
            &composite,
            "\"a\", \"b\"",
            &Some(vec![Value::Int(1), Value::Text("x".into())]),
            0,
        );
        assert!(composite_next.contains("WHERE (\"a\", \"b\") > (1, 'x')"));
    }

    #[test]
    fn no_pk_select_falls_back_to_offset() {
        let sql = build_page_select(DriverKind::Mysql, "`d`.`t`", "`c`", &[], "", &None, 2000);
        assert!(sql.contains(&format!("LIMIT {PAGE_ROWS} OFFSET 2000")));
    }

    #[test]
    fn definer_clause_is_stripped() {
        let ddl = "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`local host` SQL SECURITY DEFINER VIEW `v` AS select 1";
        let stripped = strip_mysql_definer(ddl);
        // SQL SECURITY DEFINER 是另一个合法子句，只应剥掉 DEFINER=`u`@`h`
        assert!(!stripped.contains("DEFINER="));
        assert!(stripped.contains("CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW"));
        assert_eq!(
            strip_mysql_definer("CREATE VIEW v AS select 1"),
            "CREATE VIEW v AS select 1"
        );
    }
}
