//! SQLite 元数据读取。

use std::collections::BTreeMap;

use ramag_domain::entities::{
    Column, DriverKind, ForeignKey, ForeignKeyAction, Index, Schema, Table, Trigger,
};
use ramag_domain::error::{DomainError, Result};
use ramag_infra_sql_shared::{
    METADATA_FETCH_LIMIT, ensure_metadata_item_limit, ensure_metadata_result_limit,
};
use sqlx::Row as _;
use sqlx::sqlite::SqlitePool;
use tracing::debug;

use crate::pool::map_sqlite_error_for_metadata;
use crate::types::map_column_type;

/// 读取 SQLite 引擎版本，供连接详情和诊断面板显示。
pub async fn server_version(pool: &SqlitePool) -> Result<String> {
    let row = sqlx::query("SELECT sqlite_version()")
        .fetch_one(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    row.try_get::<String, _>(0)
        .map_err(|error| DomainError::QueryFailed(format!("读取 SQLite 版本失败：{error}")))
}

/// SQLite 的 main、temp 以及已附加数据库都作为可展开的 schema 返回。
pub async fn list_schemas(pool: &SqlitePool) -> Result<Vec<Schema>> {
    debug!(
        operation = "sql_metadata_list_schemas",
        "listing sqlite schemas"
    );
    let rows = sqlx::query("SELECT name FROM pragma_database_list ORDER BY seq LIMIT ?")
        .bind(METADATA_FETCH_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "Schema")?;
    let schemas = rows
        .into_iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .map(|name| Schema {
                    name,
                    charset: None,
                    collation: None,
                })
                .map_err(|error| {
                    DomainError::QueryFailed(format!("读取 SQLite schema 失败：{error}"))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_metadata_result_limit(&schemas, "Schema")?;
    Ok(schemas)
}

/// 列出用户表和视图；SQLite 内部 sqlite_* 对象不展示在树中。
pub async fn list_tables(pool: &SqlitePool, schema: &str) -> Result<Vec<Table>> {
    debug!(
        operation = "sql_metadata_list_tables",
        ?schema,
        "listing sqlite tables"
    );
    let sql = format!(
        "SELECT name, type, sql FROM {}.sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY type, name LIMIT ?",
        quote_identifier(schema)
    );
    let rows = sqlx::query(&sql)
        .bind(METADATA_FETCH_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "表与视图")?;
    let tables = rows
        .into_iter()
        .map(|row| {
            let name = row.try_get::<String, _>(0).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 表名失败：{error}"))
            })?;
            let object_type = row.try_get::<String, _>(1).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 对象类型失败：{error}"))
            })?;
            let comment = row.try_get::<Option<String>, _>(2).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite DDL 失败：{error}"))
            })?;
            Ok(Table {
                name,
                schema: schema.to_string(),
                comment,
                is_view: object_type.eq_ignore_ascii_case("view"),
                size_bytes: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_metadata_result_limit(&tables, "表与视图")?;
    Ok(tables)
}

/// 使用 table_xinfo 保留隐藏的生成列标记，并将 SQLite affinity 映射为领域列类型。
pub async fn list_columns(pool: &SqlitePool, schema: &str, table: &str) -> Result<Vec<Column>> {
    debug!(
        operation = "sql_metadata_list_columns",
        ?schema,
        ?table,
        "listing sqlite columns"
    );
    let sql = format!(
        "PRAGMA {}.table_xinfo({})",
        quote_identifier(schema),
        quote_identifier(table)
    );
    let rows = sqlx::query(&sql)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "列")?;
    let columns = rows
        .into_iter()
        .map(|row| {
            let cid = row.try_get::<i64, _>(0).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 列序号失败：{error}"))
            })?;
            let name = row.try_get::<String, _>(1).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 列名失败：{error}"))
            })?;
            let raw_type = row.try_get::<String, _>(2).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 列类型失败：{error}"))
            })?;
            let not_null = row.try_get::<i64, _>(3).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite NOT NULL 标记失败：{error}"))
            })?;
            let default_value = row.try_get::<Option<String>, _>(4).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 默认值失败：{error}"))
            })?;
            let primary_key = row.try_get::<i64, _>(5).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 主键标记失败：{error}"))
            })?;
            let ordinal_position = u32::try_from(cid.saturating_add(1))
                .map_err(|_| DomainError::QueryFailed(format!("SQLite 列 {name} 的序号无效")))?;
            Ok(Column {
                name,
                data_type: map_column_type(&raw_type),
                nullable: not_null == 0,
                default_value,
                is_primary_key: primary_key > 0,
                comment: None,
                ordinal_position: Some(ordinal_position),
                is_auto_increment: primary_key > 0 && raw_type.eq_ignore_ascii_case("INTEGER"),
                generation_expression: None,
                generated_storage: None,
                identity_generation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_metadata_result_limit(&columns, "列")?;
    Ok(columns)
}

/// 读取索引列顺序；SQLite 自动生成的主键和唯一索引也保留在结果中。
pub async fn list_indexes(pool: &SqlitePool, schema: &str, table: &str) -> Result<Vec<Index>> {
    debug!(
        operation = "sql_metadata_list_indexes",
        ?schema,
        ?table,
        "listing sqlite indexes"
    );
    let sql = format!(
        "PRAGMA {}.index_list({})",
        quote_identifier(schema),
        quote_identifier(table)
    );
    let rows = sqlx::query(&sql)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "索引")?;

    let mut indexes = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row.try_get::<String, _>(1).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 索引名失败：{error}"))
        })?;
        let unique = row.try_get::<i64, _>(2).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 索引唯一标记失败：{error}"))
        })? != 0;
        let origin = row.try_get::<String, _>(3).unwrap_or_default();
        let column_sql = format!(
            "PRAGMA {}.index_info({})",
            quote_identifier(schema),
            quote_identifier(&name)
        );
        let column_rows = sqlx::query(&column_sql)
            .fetch_all(pool)
            .await
            .map_err(|error| map_sqlite_error_for_metadata(&error))?;
        ensure_metadata_item_limit(column_rows.len(), "索引列")?;
        let columns = column_rows
            .into_iter()
            .map(|column| {
                column
                    .try_get::<Option<String>, _>(2)
                    .map(|name| name.unwrap_or_else(|| "<expression>".to_string()))
                    .map_err(|error| {
                        DomainError::QueryFailed(format!("读取 SQLite 索引列失败：{error}"))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        indexes.push(Index {
            name,
            unique,
            primary: origin.eq_ignore_ascii_case("pk"),
            columns,
        });
    }
    ensure_metadata_result_limit(&indexes, "索引")?;
    Ok(indexes)
}

/// 按 foreign_key_list 的约束编号聚合多列外键。
pub async fn list_foreign_keys(
    pool: &SqlitePool,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>> {
    debug!(
        operation = "sql_metadata_list_foreign_keys",
        ?schema,
        ?table,
        "listing sqlite foreign keys"
    );
    let sql = format!(
        "PRAGMA {}.foreign_key_list({})",
        quote_identifier(schema),
        quote_identifier(table)
    );
    let rows = sqlx::query(&sql)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "外键列")?;
    let mut grouped: BTreeMap<i64, ForeignKey> = BTreeMap::new();
    for row in rows {
        let id = row.try_get::<i64, _>(0).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 外键编号失败：{error}"))
        })?;
        let ref_table = row.try_get::<String, _>(2).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 外键目标表失败：{error}"))
        })?;
        let column = row.try_get::<Option<String>, _>(3).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 外键列失败：{error}"))
        })?;
        let ref_column = row.try_get::<Option<String>, _>(4).map_err(|error| {
            DomainError::QueryFailed(format!("读取 SQLite 外键目标列失败：{error}"))
        })?;
        let on_update =
            parse_foreign_key_action(row.try_get::<String, _>(5).unwrap_or_default(), id, "更新")?;
        let on_delete =
            parse_foreign_key_action(row.try_get::<String, _>(6).unwrap_or_default(), id, "删除")?;
        let entry = grouped.entry(id).or_insert_with(|| ForeignKey {
            name: format!("fk_{id}"),
            columns: Vec::new(),
            ref_schema: schema.to_string(),
            ref_table,
            ref_columns: Vec::new(),
            on_delete,
            on_update,
        });
        if let Some(column) = column {
            entry.columns.push(column);
        }
        if let Some(ref_column) = ref_column {
            entry.ref_columns.push(ref_column);
        }
    }
    let foreign_keys = grouped.into_values().collect::<Vec<_>>();
    ensure_metadata_result_limit(&foreign_keys, "外键")?;
    Ok(foreign_keys)
}

/// 读取触发器定义；timing/event 从 SQLite 保存的定义中提取用于列表展示。
pub async fn list_triggers(pool: &SqlitePool, schema: &str, table: &str) -> Result<Vec<Trigger>> {
    debug!(
        operation = "sql_metadata_list_triggers",
        ?schema,
        ?table,
        "listing sqlite triggers"
    );
    let sql = format!(
        "SELECT name, sql FROM {}.sqlite_schema WHERE type = 'trigger' AND tbl_name = ? ORDER BY name LIMIT ?",
        quote_identifier(schema)
    );
    let rows = sqlx::query(&sql)
        .bind(table)
        .bind(METADATA_FETCH_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(|error| map_sqlite_error_for_metadata(&error))?;
    ensure_metadata_item_limit(rows.len(), "触发器")?;
    let triggers = rows
        .into_iter()
        .map(|row| {
            let name = row.try_get::<String, _>(0).map_err(|error| {
                DomainError::QueryFailed(format!("读取 SQLite 触发器名失败：{error}"))
            })?;
            let definition = row
                .try_get::<Option<String>, _>(1)
                .map_err(|error| {
                    DomainError::QueryFailed(format!("读取 SQLite 触发器定义失败：{error}"))
                })?
                .unwrap_or_default();
            Ok(Trigger {
                name,
                timing: trigger_timing(&definition),
                event: trigger_event(&definition),
                definition,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_metadata_result_limit(&triggers, "触发器")?;
    Ok(triggers)
}

fn quote_identifier(value: &str) -> String {
    DriverKind::Sqlite.quote_identifier(value)
}

fn parse_foreign_key_action(value: String, id: i64, phase: &str) -> Result<ForeignKeyAction> {
    ForeignKeyAction::parse_sql(&value).ok_or_else(|| {
        DomainError::QueryFailed(format!(
            "SQLite 外键 {id} 的 ON {phase} 规则无法识别：{value}"
        ))
    })
}

fn trigger_timing(definition: &str) -> String {
    let upper = definition.to_ascii_uppercase();
    ["BEFORE", "AFTER", "INSTEAD OF"]
        .iter()
        .find(|value| upper.contains(**value))
        .map_or_else(String::new, |value| (*value).to_string())
}

fn trigger_event(definition: &str) -> String {
    let upper = definition.to_ascii_uppercase();
    ["INSERT", "UPDATE", "DELETE"]
        .iter()
        .find(|value| upper.contains(**value))
        .map_or_else(String::new, |value| (*value).to_string())
}
