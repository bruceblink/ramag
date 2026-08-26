//! PostgreSQL 元数据读取。优先使用 information_schema，索引和列注释使用 pg_catalog。

use ramag_domain::entities::{Column, ForeignKey, ForeignKeyAction, Index, Schema, Table};
use ramag_domain::error::{DomainError, Result};
use ramag_infra_sql_shared::{
    METADATA_FETCH_LIMIT, ensure_metadata_item_limit, ensure_metadata_result_limit,
};
use sqlx::PgPool;
use tracing::debug;

use crate::errors::map_postgres_error;
use crate::types::map_column_kind;

/// 返回全部模式，包括系统模式；展示层负责过滤。
pub async fn list_schemas(pool: &PgPool) -> Result<Vec<Schema>> {
    debug!(operation = "sql_metadata_list_schemas", "listing schemas");

    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT schema_name, default_character_set_name
        FROM information_schema.schemata
        ORDER BY schema_name
        LIMIT $1
        "#,
    )
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "Schema")?;

    let schemas = rows
        .into_iter()
        .map(|(name, charset)| Schema {
            name,
            charset,
            // PG 的 collation 是列 / 表级，schema 无此概念
            collation: None,
        })
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&schemas, "Schema")?;
    Ok(schemas)
}

/// 列出普通表、视图和物化视图。
pub async fn list_tables(pool: &PgPool, schema: &str) -> Result<Vec<Table>> {
    debug!(
        operation = "sql_metadata_list_tables",
        ?schema,
        "listing tables"
    );

    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            t.table_name::text,
            t.table_type::text,
            LEFT(obj_description(c.oid, 'pg_class'), 4096) AS table_comment
        FROM information_schema.tables t
        LEFT JOIN pg_namespace n ON n.nspname = t.table_schema
        LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name
        WHERE t.table_schema = $1
          AND t.table_type IN ('BASE TABLE', 'VIEW')
        UNION ALL
        SELECT
            mv.matviewname::text AS table_name,
            'MATERIALIZED VIEW'::text AS table_type,
            LEFT(obj_description(c.oid, 'pg_class'), 4096) AS table_comment
        FROM pg_matviews mv
        LEFT JOIN pg_namespace n ON n.nspname = mv.schemaname
        LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = mv.matviewname
        WHERE mv.schemaname = $1
        ORDER BY 2, 1
        LIMIT $2
        "#,
    )
    .bind(schema)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
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

/// COLUMNS 一行：column_name / data_type / udt_name / default / comment / char_max_len / nullable
type PgColumnRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i32>,
    bool,
    String,
);

/// 列注释走 pg_catalog.col_description，其他走 information_schema.columns
pub async fn list_columns(pool: &PgPool, schema: &str, table: &str) -> Result<Vec<Column>> {
    debug!(
        operation = "sql_metadata_list_columns",
        ?schema,
        ?table,
        "listing columns"
    );

    let rows: Vec<PgColumnRow> = sqlx::query_as(
        r#"
        WITH sync_settings AS (
            SELECT set_config('search_path', 'pg_catalog', true) AS configured
        )
        SELECT
            c.column_name::text,
            c.data_type::text,
            c.udt_name::text,
            LEFT(c.column_default, 4096),
            LEFT(col_description(pgc.oid, c.ordinal_position::int), 4096) AS column_comment,
            c.character_maximum_length::int,
            (c.is_nullable = 'YES') AS nullable,
            (
                pg_catalog.format_type(a.atttypid, a.atttypmod)
                || substr(sync_settings.configured, 1, 0)
            )::text AS exact_type
        FROM information_schema.columns c
        LEFT JOIN pg_namespace n ON n.nspname = c.table_schema
        LEFT JOIN pg_class pgc ON pgc.relnamespace = n.oid AND pgc.relname = c.table_name
        JOIN pg_attribute a ON a.attrelid = pgc.oid AND a.attname = c.column_name
        CROSS JOIN sync_settings
        WHERE c.table_schema = $1 AND c.table_name = $2
        ORDER BY c.ordinal_position
        LIMIT $3
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "列")?;

    // 主键列需要通过约束信息单独查询。
    let pk_cols: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT kcu.column_name::text
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_catalog = kcu.constraint_catalog
         AND tc.table_schema = kcu.table_schema
         AND tc.table_name = kcu.table_name
         AND tc.constraint_name = kcu.constraint_name
        WHERE tc.constraint_type = 'PRIMARY KEY'
          AND tc.table_schema = $1 AND tc.table_name = $2
        LIMIT $3
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
    ensure_metadata_item_limit(pk_cols.len(), "主键列")?;
    let pk_names: std::collections::HashSet<String> = pk_cols.into_iter().map(|(n,)| n).collect();

    let columns = rows
        .into_iter()
        .map(
            |(
                name,
                data_type,
                udt_name,
                default_value,
                comment,
                char_max_len,
                nullable,
                exact_type,
            )| {
                // format_type 保留 numeric 精度、timestamp 精度、数组和自定义类型限定。
                let full_type = if exact_type.is_empty() {
                    compose_full_type(&data_type, &udt_name, char_max_len)
                } else {
                    exact_type
                };
                Column {
                    name: name.clone(),
                    data_type: map_column_kind(&data_type, &full_type),
                    nullable,
                    default_value,
                    is_primary_key: pk_names.contains(&name),
                    comment: comment.filter(|c| !c.is_empty()),
                }
            },
        )
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&columns, "列")?;
    Ok(columns)
}

/// 组合完整类型，例如将 varchar 和长度 255 组合为 varchar(255)。
fn compose_full_type(data_type: &str, udt: &str, char_max: Option<i32>) -> String {
    let base = if udt.is_empty() { data_type } else { udt };
    if let Some(n) = char_max {
        format!("{base}({n})")
    } else {
        base.to_string()
    }
}

/// 列出所有索引方法创建的索引，包括表达式索引。
pub async fn list_indexes(pool: &PgPool, schema: &str, table: &str) -> Result<Vec<Index>> {
    debug!(
        operation = "sql_metadata_list_indexes",
        ?schema,
        ?table,
        "listing indexes"
    );

    let rows: Vec<(String, bool, bool, Vec<String>)> = sqlx::query_as(
        r#"
        SELECT
            i.relname::text AS index_name,
            ix.indisunique AS is_unique,
            ix.indisprimary AS is_primary,
            array_agg(
                pg_get_indexdef(ix.indexrelid, key.ordinality::int, true)
                ORDER BY key.ordinality
            ) AS columns
        FROM pg_index ix
        JOIN pg_class i ON i.oid = ix.indexrelid
        JOIN pg_class t ON t.oid = ix.indrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS key(attnum, ordinality)
          ON key.ordinality <= ix.indnkeyatts
        WHERE n.nspname = $1 AND t.relname = $2
        GROUP BY i.relname, ix.indisunique, ix.indisprimary
        ORDER BY ix.indisprimary DESC, i.relname
        LIMIT $3
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "索引")?;

    let indexes = rows
        .into_iter()
        .map(|(name, unique, primary, columns)| Index {
            name,
            unique,
            primary,
            columns,
        })
        .collect::<Vec<_>>();
    ensure_metadata_result_limit(&indexes, "索引")?;
    Ok(indexes)
}

pub async fn list_foreign_keys(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>> {
    debug!(
        operation = "sql_metadata_list_foreign_keys",
        ?schema,
        ?table,
        "listing foreign keys"
    );

    let rows: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
        r#"
        SELECT
            con.conname::text,
            local_column.attname::text,
            ref_schema.nspname::text,
            ref_table.relname::text,
            ref_column.attname::text,
            CASE con.confupdtype
                WHEN 'a' THEN 'NO ACTION'
                WHEN 'r' THEN 'RESTRICT'
                WHEN 'c' THEN 'CASCADE'
                WHEN 'n' THEN 'SET NULL'
                WHEN 'd' THEN 'SET DEFAULT'
                ELSE 'UNKNOWN'
            END,
            CASE con.confdeltype
                WHEN 'a' THEN 'NO ACTION'
                WHEN 'r' THEN 'RESTRICT'
                WHEN 'c' THEN 'CASCADE'
                WHEN 'n' THEN 'SET NULL'
                WHEN 'd' THEN 'SET DEFAULT'
                ELSE 'UNKNOWN'
            END
        FROM pg_constraint con
        JOIN pg_class local_table ON local_table.oid = con.conrelid
        JOIN pg_namespace local_schema ON local_schema.oid = local_table.relnamespace
        JOIN pg_class ref_table ON ref_table.oid = con.confrelid
        JOIN pg_namespace ref_schema ON ref_schema.oid = ref_table.relnamespace
        JOIN LATERAL generate_subscripts(con.conkey, 1) AS key(position) ON true
        JOIN pg_attribute local_column
          ON local_column.attrelid = local_table.oid
         AND local_column.attnum = con.conkey[key.position]
        JOIN pg_attribute ref_column
          ON ref_column.attrelid = ref_table.oid
         AND ref_column.attnum = con.confkey[key.position]
        WHERE con.contype = 'f'
          AND local_schema.nspname = $1 AND local_table.relname = $2
        ORDER BY con.conname, key.position
        LIMIT $3
        "#,
    )
    .bind(schema)
    .bind(table)
    .bind(METADATA_FETCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| map_postgres_error(&e))?;
    ensure_metadata_item_limit(rows.len(), "外键列")?;

    let mut grouped: std::collections::BTreeMap<String, ForeignKey> =
        std::collections::BTreeMap::new();
    for (name, col, ref_schema, ref_table, ref_col, update_rule, delete_rule) in rows {
        let on_update = ForeignKeyAction::parse_sql(&update_rule).ok_or_else(|| {
            DomainError::QueryFailed(format!("PostgreSQL 外键 {name} 的 ON UPDATE 规则无法识别"))
        })?;
        let on_delete = ForeignKeyAction::parse_sql(&delete_rule).ok_or_else(|| {
            DomainError::QueryFailed(format!("PostgreSQL 外键 {name} 的 ON DELETE 规则无法识别"))
        })?;
        let entry = grouped.entry(name.clone()).or_insert_with(|| ForeignKey {
            name,
            columns: Vec::new(),
            ref_schema,
            ref_table,
            ref_columns: Vec::new(),
            on_delete,
            on_update,
        });
        entry.columns.push(col);
        entry.ref_columns.push(ref_col);
    }
    let foreign_keys = grouped.into_values().collect::<Vec<_>>();
    ensure_metadata_result_limit(&foreign_keys, "外键")?;
    Ok(foreign_keys)
}

/// `SHOW server_version`，形如 "13.5"
pub async fn server_version(pool: &PgPool) -> Result<String> {
    let (v,): (String,) = sqlx::query_as("SHOW server_version")
        .fetch_one(pool)
        .await
        .map_err(|e| map_postgres_error(&e))?;
    Ok(v)
}
