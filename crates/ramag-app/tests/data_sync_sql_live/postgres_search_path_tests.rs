//! SQL 数据同步集成测试分组。

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn postgres_sync_qualifies_enum_types_from_search_path_schema() {
    let Some((source, target)) = postgres_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 后运行 SQL 同步测试");
        return;
    };
    let suffix = std::process::id();
    let enum_name = format!("ramag_sync_state_{suffix}");
    let table_name = format!("ramag_sync_enum_defaults_{suffix}");
    let target_schema = format!("ramag_sync_enum_qualified_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; \
             DROP TABLE IF EXISTS public.\"{table_name}\"; \
             DROP TYPE IF EXISTS public.\"{enum_name}\"; \
             CREATE TYPE public.\"{enum_name}\" AS ENUM ('new','active','archived'); \
             CREATE TABLE public.\"{table_name}\" (\
                 \"id\" BIGINT PRIMARY KEY, \
                 \"state\" public.\"{enum_name}\" NOT NULL DEFAULT 'new', \
                 \"states\" public.\"{enum_name}\"[] NOT NULL \
                     DEFAULT ARRAY['new'::public.\"{enum_name}\",'active'::public.\"{enum_name}\"]\
             ); \
             INSERT INTO public.\"{table_name}\" (\"id\") VALUES (1);"
        ),
    )
    .await;

    let summary = execute_sync(
        &sync,
        &gate,
        sql_request(
            &source,
            &target,
            "public",
            &target_schema,
            &[(&table_name, &table_name)],
        ),
        DataSyncConfirmation::CreateMissingTargets,
    )
    .await;
    assert_eq!(summary.inserted, 1);
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!(
                "SELECT string_agg(DISTINCT type_ns.nspname, ',') \
                   FROM pg_attribute a \
                   JOIN pg_class c ON c.oid=a.attrelid \
                   JOIN pg_namespace table_ns ON table_ns.oid=c.relnamespace \
                   JOIN pg_type column_type ON column_type.oid=a.atttypid \
                   JOIN pg_type base_type ON base_type.oid=CASE \
                       WHEN column_type.typelem=0 THEN column_type.oid ELSE column_type.typelem END \
                   JOIN pg_namespace type_ns ON type_ns.oid=base_type.typnamespace \
                  WHERE table_ns.nspname='{target_schema}' AND c.relname='{table_name}' \
                    AND a.attname IN ('state','states');"
            ),
        )
        .await,
        target_schema
    );
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!(
                "SELECT \"state\"::text || '|' || array_to_string(\"states\", ',') \
                 FROM \"{target_schema}\".\"{table_name}\" WHERE \"id\"=1;"
            ),
        )
        .await,
        "new|new,active"
    );

    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; \
             DROP TABLE IF EXISTS public.\"{table_name}\"; \
             DROP TYPE IF EXISTS public.\"{enum_name}\";"
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_sync_seeded_schema_to_distinct_database() {
    let Some((source, target)) = postgres_distinct_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_DISTINCT_* 后运行跨 PostgreSQL 数据库同步测试");
        return;
    };
    let target_schema = format!("ramag_sync_distinct_{}", std::process::id());
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &target,
        format!("DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE;"),
    )
    .await;

    let request = sql_request(
        &source,
        &target,
        "public",
        &target_schema,
        &[
            ("bulk_records", "bulk_records"),
            ("large_values", "large_values"),
            ("type_matrix", "type_matrix"),
        ],
    );
    let summary = execute_sync(
        &sync,
        &gate,
        request,
        DataSyncConfirmation::CreateMissingTargets,
    )
    .await;
    assert_eq!(summary.inserted, 100_004);
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM \"{target_schema}\".\"type_matrix\";"),
        )
        .await,
        3
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM \"{target_schema}\".\"bulk_records\";"),
        )
        .await,
        100_000
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!(
                "SELECT octet_length(\"bytea_value\") FROM \"{target_schema}\".\"large_values\" WHERE \"id\"=1;"
            ),
        )
        .await,
        1_048_576
    );

    exec(
        &sql,
        &target,
        format!("DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE;"),
    )
    .await;
}
