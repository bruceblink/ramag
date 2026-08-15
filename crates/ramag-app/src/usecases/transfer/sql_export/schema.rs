use super::super::sql_catalog::{
    PgSequenceInfo, begin_marker, parse_show_create, pg_comments_query, pg_table_create_query,
};
use super::super::{ExportSink, MYSQL_IMPORT_PREFIX};
use super::{run_first_column, write_sql_statement};
use crate::usecases::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, DriverKind, Query, Table, build_ddl_query};
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

pub(super) async fn write_header(
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
            // 保留源库编码和排序规则。
            let source_schema = match svc.list_schemas(config).await {
                Ok(schemas) => schemas.into_iter().find(|item| item.name == schema),
                Err(error) => {
                    warn!(
                        operation = "sql_export_schema_options",
                        connection_id = %config.id,
                        driver = ?driver,
                        schema,
                        error = %error,
                        "load source schema options failed"
                    );
                    None
                }
            };
            let charset_clause = source_schema
                .map(|source_schema| {
                    let mut clause = String::new();
                    if let Some(charset) = source_schema.charset {
                        clause.push_str(&format!(" DEFAULT CHARACTER SET {charset}"));
                    }
                    if let Some(collation) = source_schema.collation {
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

pub(super) async fn write_table_ddl(
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
            // 覆盖导入会级联删除 OWNED 序列，需随表重建。
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
