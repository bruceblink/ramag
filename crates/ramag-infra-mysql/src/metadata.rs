//! 元数据查询：基于 INFORMATION_SCHEMA，避免 SHOW 语法的版本差异。
//! 字符串列统一 `CONVERT(... USING utf8mb4)`，避开 sqlx 把某些环境的回包识为 VARBINARY 导致解码失败

use ramag_domain::entities::{Column, ForeignKey, Index, Schema, Table};
use ramag_domain::error::Result;
use ramag_infra_sql_shared::{
    METADATA_FETCH_LIMIT, ensure_metadata_item_limit, ensure_metadata_result_limit,
};
use sqlx::MySqlPool;
use tracing::debug;

use crate::errors::map_mysql_error;
use crate::types::map_column_type;

/// 含系统库（mysql / information_schema / performance_schema / sys）；过滤交给 UI
pub async fn list_schemas(pool: &MySqlPool) -> Result<Vec<Schema>> {
    debug!("list_schemas");

    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            CONVERT(SCHEMA_NAME USING utf8mb4),
            CONVERT(DEFAULT_CHARACTER_SET_NAME USING utf8mb4),
            CONVERT(DEFAULT_COLLATION_NAME USING utf8mb4)
        FROM information_schema.SCHEMATA
        ORDER BY SCHEMA_NAME
        LIMIT ?
        "#,
    )
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_mysql_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "Schema")?;

    let schemas = rows
        .into_iter()
        .map(|(name, charset, collation)| Schema {
            name,
            charset,
            collation,
        })
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&schemas, "Schema")?;
    Ok(schemas)
}

/// 列出 BASE TABLE / VIEW / SYSTEM VIEW。后两者在 UI 都归为视图分组
pub async fn list_tables(pool: &MySqlPool, schema: &str) -> Result<Vec<Table>> {
    debug!(?schema, "list_tables");

    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            CONVERT(TABLE_NAME USING utf8mb4),
            CONVERT(TABLE_TYPE USING utf8mb4),
            LEFT(CONVERT(TABLE_COMMENT USING utf8mb4), 4096)
        FROM information_schema.TABLES
        WHERE TABLE_SCHEMA = ? AND TABLE_TYPE IN ('BASE TABLE', 'VIEW', 'SYSTEM VIEW')
        ORDER BY TABLE_TYPE, TABLE_NAME
        LIMIT ?
        "#,
    )
    .bind(schema)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_mysql_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "表与视图")?;

    let tables = rows
        .into_iter()
        .map(|(name, table_type, comment)| {
            let is_view = !table_type.eq_ignore_ascii_case("BASE TABLE");
            Table {
                name,
                schema: schema.to_string(),
                comment: comment.filter(|c| !c.is_empty()),
                is_view,
            }
        })
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&tables, "表与视图")?;
    Ok(tables)
}

/// COLUMNS 一行：name / data_type / column_type / is_nullable / column_default / column_comment / column_key
type ColumnRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

/// 列出指定表的所有列
pub async fn list_columns(pool: &MySqlPool, schema: &str, table: &str) -> Result<Vec<Column>> {
    debug!(?schema, ?table, "list_columns");

    let rows: Vec<ColumnRow> = sqlx::query_as(
        r#"
            SELECT
                CONVERT(COLUMN_NAME USING utf8mb4),
                CONVERT(DATA_TYPE USING utf8mb4),
                CONVERT(COLUMN_TYPE USING utf8mb4),
                CONVERT(IS_NULLABLE USING utf8mb4),
                LEFT(CONVERT(COLUMN_DEFAULT USING utf8mb4), 4096),
                LEFT(CONVERT(COLUMN_COMMENT USING utf8mb4), 4096),
                CONVERT(COLUMN_KEY USING utf8mb4)
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
            ORDER BY ORDINAL_POSITION
            LIMIT ?
            "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_mysql_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "列")?;

    let columns = rows
        .into_iter()
        .map(
            |(name, data_type, column_type, is_nullable, default_value, comment, column_key)| {
                Column {
                    name,
                    data_type: map_column_type(&data_type, &column_type),
                    nullable: is_nullable.eq_ignore_ascii_case("YES"),
                    default_value,
                    is_primary_key: column_key == "PRI",
                    comment: comment.filter(|c| !c.is_empty()),
                }
            },
        )
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&columns, "列")?;
    Ok(columns)
}

/// 含主键 / 唯一 / 普通索引。基于 STATISTICS 一行一列，按 INDEX_NAME 聚合
pub async fn list_indexes(pool: &MySqlPool, schema: &str, table: &str) -> Result<Vec<Index>> {
    debug!(?schema, ?table, "list_indexes");

    let rows: Vec<(String, i64, i64, String)> = sqlx::query_as(
        r#"
        SELECT
            CONVERT(INDEX_NAME USING utf8mb4),
            CAST(NON_UNIQUE AS SIGNED),
            CAST(SEQ_IN_INDEX AS SIGNED),
            CONVERT(COLUMN_NAME USING utf8mb4)
        FROM information_schema.STATISTICS
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
        ORDER BY INDEX_NAME, SEQ_IN_INDEX
        LIMIT ?
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_mysql_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "索引列")?;

    let mut grouped: std::collections::BTreeMap<String, Index> = std::collections::BTreeMap::new();
    for (idx_name, non_unique, _seq, col_name) in rows {
        let primary = idx_name == "PRIMARY";
        let entry = grouped.entry(idx_name.clone()).or_insert_with(|| Index {
            name: idx_name,
            unique: non_unique == 0,
            primary,
            columns: Vec::new(),
        });
        entry.columns.push(col_name);
    }

    // 主键置顶，其余按名
    let mut indexes: Vec<Index> = grouped.into_values().collect();
    indexes.sort_by(|a, b| match (a.primary, b.primary) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    ensure_metadata_result_limit(&indexes, "索引")?;
    Ok(indexes)
}

/// 基于 KEY_COLUMN_USAGE
pub async fn list_foreign_keys(
    pool: &MySqlPool,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>> {
    debug!(?schema, ?table, "list_foreign_keys");

    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        r#"
        SELECT
            CONVERT(CONSTRAINT_NAME USING utf8mb4),
            CONVERT(COLUMN_NAME USING utf8mb4),
            CONVERT(REFERENCED_TABLE_SCHEMA USING utf8mb4),
            CONVERT(REFERENCED_TABLE_NAME USING utf8mb4),
            CONVERT(REFERENCED_COLUMN_NAME USING utf8mb4)
        FROM information_schema.KEY_COLUMN_USAGE
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
          AND REFERENCED_TABLE_NAME IS NOT NULL
        ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION
        LIMIT ?
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_mysql_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "外键列")?;

    let mut grouped: std::collections::BTreeMap<String, ForeignKey> =
        std::collections::BTreeMap::new();
    for (name, col, ref_schema, ref_table, ref_col) in rows {
        let entry = grouped.entry(name.clone()).or_insert_with(|| ForeignKey {
            name,
            columns: Vec::new(),
            ref_schema,
            ref_table,
            ref_columns: Vec::new(),
        });
        entry.columns.push(col);
        entry.ref_columns.push(ref_col);
    }
    let foreign_keys = grouped.into_values().collect::<Vec<_>>();
    ensure_metadata_result_limit(&foreign_keys, "外键")?;
    Ok(foreign_keys)
}

/// `SELECT VERSION()`，形如 "8.0.32"
pub async fn server_version(pool: &MySqlPool) -> Result<String> {
    let (v,): (String,) = sqlx::query_as("SELECT VERSION()")
        .fetch_one(pool)
        .await
        .map_err(|e| map_mysql_error(&e))?;
    Ok(v)
}
