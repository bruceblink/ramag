//! 集成测试：连接真实 PostgreSQL。缺 RAMAG_TEST_PG_* 环境变量时跳过。PG 必须指定 db

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, DriverKind, ForeignKeyAction, Query, Value};
use ramag_domain::traits::Driver;
use ramag_infra_postgres::PostgresDriver;

/// 缺任一字段就跳过测试。PG 必须指定 database，`RAMAG_TEST_PG_DB` 必填
fn config_from_env() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_PG_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_PG_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_PG_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_PG_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_PG_DB").ok()?;

    Some(ConnectionConfig {
        driver: DriverKind::Postgres,
        password,
        database: Some(database),
        ..ConnectionConfig::new_mysql("integration-test", host, port, user)
    })
}

fn seeded_dataset_enabled() -> bool {
    std::env::var("RAMAG_TEST_DATASET").as_deref() == Ok("full")
}

/// 缺环境变量时打印 skip 提示再 return
macro_rules! require_env {
    () => {{
        match config_from_env() {
            Some(c) => c,
            None => {
                eprintln!(
                    "[SKIP] integration test skipped: 设置 RAMAG_TEST_PG_* 环境变量后运行"
                );
                return;
            }
        }
    }};
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_works() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    driver
        .test_connection(&config)
        .await
        .expect("test_connection 失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_version_returns_value() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let v = driver
        .server_version(&config)
        .await
        .expect("server_version 失败");
    println!("postgres version: {v}");
    assert!(!v.is_empty(), "版本字符串应非空");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_schemas_returns_data() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let schemas = driver
        .list_schemas(&config)
        .await
        .expect("list_schemas 失败");
    println!("schemas: {:#?}", schemas);
    // PG 默认有 public
    assert!(
        schemas.iter().any(|s| s.name == "public"),
        "应包含 public schema"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_tables_for_public() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let tables = driver
        .list_tables(&config, "public")
        .await
        .expect("list_tables 失败");
    println!("tables in public: {:#?}", tables);
    assert!(
        tables
            .iter()
            .filter(|table| !table.is_view)
            .all(|table| table.size_bytes.is_some()),
        "普通表应返回物理大小"
    );
    assert!(
        tables
            .iter()
            .filter(|table| table.is_view)
            .all(|table| table.size_bytes.is_none()),
        "视图不应伪造表大小"
    );
}

/// 验证结构对比依赖的列、索引和外键元数据能从真实 PostgreSQL 读取。
#[tokio::test(flavor = "multi_thread")]
async fn list_schema_metadata_for_comparison_tables() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let schema = "public";
    let suffix = std::process::id();
    let source = format!("ramag_metadata_source_{suffix}");
    let target = format!("ramag_metadata_target_{suffix}");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS \"{target}\"; DROP TABLE IF EXISTS \"{source}\"; \
                 CREATE TABLE \"{source}\" (id integer NOT NULL PRIMARY KEY, name varchar(64) NOT NULL); \
                 CREATE INDEX \"idx_metadata_source_name\" ON \"{source}\" (name)"
            )),
        )
        .await
        .expect("创建 PostgreSQL 元数据源表失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "CREATE TABLE \"{target}\" (id integer NOT NULL PRIMARY KEY, name varchar(128) NOT NULL, email varchar(128)); \
                 CREATE INDEX \"idx_metadata_target_email\" ON \"{target}\" (email); \
                 ALTER TABLE \"{target}\" ADD CONSTRAINT \"fk_metadata_target_source\" \
                 FOREIGN KEY (id) REFERENCES \"{source}\" (id) ON DELETE CASCADE ON UPDATE CASCADE"
            )),
        )
        .await
        .expect("创建 PostgreSQL 元数据目标表失败");

    let source_columns = driver
        .list_columns(&config, schema, &source)
        .await
        .expect("读取 PostgreSQL 源表列失败");
    let target_indexes = driver
        .list_indexes(&config, schema, &target)
        .await
        .expect("读取 PostgreSQL 目标表索引失败");
    let target_foreign_keys = driver
        .list_foreign_keys(&config, schema, &target)
        .await
        .expect("读取 PostgreSQL 目标表外键失败");

    assert!(source_columns.iter().any(|column| column.name == "name"));
    assert!(
        target_indexes
            .iter()
            .any(|index| index.name == "idx_metadata_target_email")
    );
    assert!(target_foreign_keys.iter().any(|foreign_key| {
        foreign_key.name == "fk_metadata_target_source"
            && foreign_key.ref_table == source
            && foreign_key.columns == ["id"]
            && foreign_key.ref_columns == ["id"]
            && foreign_key.on_delete == ForeignKeyAction::Cascade
            && foreign_key.on_update == ForeignKeyAction::Cascade
    }));

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS \"{target}\"; DROP TABLE IF EXISTS \"{source}\";"
            )),
        )
        .await
        .expect("清理 PostgreSQL 元数据测试表失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_select_one() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let result = driver
        .execute(&config, &Query::new("SELECT 1 AS one, 'hello' AS greet"))
        .await
        .expect("execute 失败");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns.len(), 2);
    assert_eq!(result.affected_rows, 0);
    println!(
        "result: cols={:?}, rows={:?}, elapsed={}ms",
        result.columns, result.rows, result.elapsed_ms
    );
}

/// Verifies that the PostgreSQL driver exposes EXPLAIN output as a normal result set.
#[tokio::test(flavor = "multi_thread")]
async fn explain_select_returns_plan_rows() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let result = driver
        .execute(&config, &Query::new("EXPLAIN SELECT 1 AS one"))
        .await
        .expect("EXPLAIN 执行失败");

    assert!(!result.columns.is_empty(), "EXPLAIN 应返回计划列");
    assert!(!result.rows.is_empty(), "EXPLAIN 应返回至少一行计划");
}

/// Verifies that a backend ID exposed by a cancellable query can stop that query.
#[tokio::test(flavor = "multi_thread")]
async fn cancellable_query_can_be_stopped_by_backend_id() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let handle = Arc::new(AtomicU64::new(0));
    let query_handle = handle.clone();
    let worker_driver = driver.clone();
    let worker_config = config.clone();
    let mut task = tokio::spawn(async move {
        worker_driver
            .execute_cancellable(
                &worker_config,
                &Query::new("SELECT pg_sleep(30)"),
                query_handle,
            )
            .await
    });

    let backend_id = match tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let id = handle.load(Ordering::SeqCst);
            if id != 0 {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    {
        Ok(id) => id,
        Err(_) => {
            task.abort();
            let _ = task.await;
            panic!("PostgreSQL 查询未及时暴露后端进程 ID");
        }
    };

    let cancel_result = tokio::time::timeout(
        Duration::from_secs(5),
        driver.cancel_query(&config, backend_id),
    )
    .await;
    assert!(
        cancel_result.is_ok(),
        "PostgreSQL 后端取消请求超时：{backend_id}"
    );
    cancel_result
        .expect("PostgreSQL 后端取消请求超时")
        .expect("PostgreSQL 后端取消请求失败");

    let execution = tokio::time::timeout(Duration::from_secs(10), &mut task)
        .await
        .expect("PostgreSQL 查询取消后仍未结束")
        .expect("PostgreSQL 查询任务异常退出");
    assert!(execution.is_err(), "被取消的 PostgreSQL 查询不应成功返回");
}

/// 验证手动事务中的写入可在同一会话内读取，并分别支持回滚和提交。
#[tokio::test(flavor = "multi_thread")]
async fn manual_transaction_commit_and_rollback() {
    let config = require_env!();
    let driver = PostgresDriver::new();
    let table = format!("ramag_transaction_probe_{}", std::process::id());
    let quoted_table = format!("\"{table}\"");
    let qualified_table = format!("public.{quoted_table}");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {qualified_table}; CREATE TABLE {qualified_table} (id integer PRIMARY KEY, value integer NOT NULL)"
            )),
        )
        .await
        .expect("创建 PostgreSQL 事务测试表失败");
    let created = driver
        .execute(
            &config,
            &Query::new(format!(
                "SELECT to_regclass('public.{table}') IS NOT NULL AS created_table"
            )),
        )
        .await
        .expect("检查 PostgreSQL 事务测试表失败");
    assert!(
        matches!(created.rows[0].values.first(), Some(Value::Bool(true))),
        "事务测试表未创建：{:?}",
        created.rows
    );

    let rollback_id = driver
        .begin_transaction(&config)
        .await
        .expect("开启 PostgreSQL 回滚事务失败");
    let inserted = driver
        .execute_in_transaction(
            &config,
            &rollback_id,
            &Query::new(format!("INSERT INTO {qualified_table} VALUES (1, 10)")),
        )
        .await
        .expect("PostgreSQL 事务内插入失败");
    assert_eq!(inserted.affected_rows, 1);
    let inside = driver
        .execute_in_transaction(
            &config,
            &rollback_id,
            &Query::new(format!("SELECT value FROM {qualified_table} WHERE id = 1")),
        )
        .await
        .expect("PostgreSQL 事务内读取失败");
    assert!(matches!(
        inside.rows[0].values.first(),
        Some(Value::Int(10))
    ));
    driver
        .rollback_transaction(&config, &rollback_id)
        .await
        .expect("PostgreSQL 回滚事务失败");

    let committed_id = driver
        .begin_transaction(&config)
        .await
        .expect("开启 PostgreSQL 提交事务失败");
    driver
        .execute_in_transaction(
            &config,
            &committed_id,
            &Query::new(format!("INSERT INTO {qualified_table} VALUES (2, 20)")),
        )
        .await
        .expect("PostgreSQL 提交事务内插入失败");
    driver
        .commit_transaction(&config, &committed_id)
        .await
        .expect("PostgreSQL 提交事务失败");

    let outside = driver
        .execute(
            &config,
            &Query::new(format!(
                "SELECT id, value FROM {qualified_table} ORDER BY id"
            )),
        )
        .await
        .expect("读取 PostgreSQL 事务最终结果失败");
    assert_eq!(outside.rows.len(), 1);
    assert!(matches!(
        outside.rows[0].values.first(),
        Some(Value::Int(2))
    ));
    assert!(matches!(
        outside.rows[0].values.get(1),
        Some(Value::Int(20))
    ));

    driver
        .execute(
            &config,
            &Query::new(format!("DROP TABLE IF EXISTS {qualified_table}")),
        )
        .await
        .expect("清理 PostgreSQL 事务测试表失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_select_with_pg_types() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let result = driver
        .execute(
            &config,
            &Query::new(
                "SELECT \
                    true AS b, \
                    42::int4 AS i, \
                    1.5::float8 AS f, \
                    1234567890123456789012.34::numeric AS n, \
                    'text'::text AS t, \
                    NULL AS null_col, \
                    NOW() AS ts, \
                    '{\"k\": \"v\"}'::jsonb AS j, \
                    '11111111-1111-1111-1111-111111111111'::uuid AS u",
            ),
        )
        .await
        .expect("execute 失败");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns.len(), 9);
    println!("typed result: {:#?}", result.rows[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_select_with_pg_native_types() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let result = driver
        .execute(
            &config,
            &Query::new(
                "SELECT \
                    '23:59:59.999999+08'::timetz AS tz, \
                    '1 day 02:03:04.000005'::interval AS iv, \
                    ARRAY[1, 2, NULL]::int4[] AS ints, \
                    ARRAY['a', '中文', NULL]::text[] AS texts, \
                    '[1,10)'::int4range AS range_value, \
                    '2001:db8::1/64'::inet AS inet_value, \
                    '10.0.0.0/8'::cidr AS cidr_value, \
                    '08:00:2b:01:02:03'::macaddr AS mac_value, \
                    B'10101010'::bit(8) AS bit_value, \
                    B'10101'::varbit AS varbit_value, \
                    '<root><item>数据</item></root>'::xml AS xml_value, \
                    to_tsvector('simple', 'ramag database') AS search_value",
            ),
        )
        .await
        .expect("PG 原生类型查询失败");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns.len(), 12);
    assert!(
        result.rows[0].values[..11]
            .iter()
            .all(|value| matches!(value, Value::Text(_))),
        "前 11 个原生类型应转换为可读文本：{:?}",
        result.rows[0]
    );
    assert!(
        matches!(&result.rows[0].values[11], Value::Bytes(value) if !value.is_empty()),
        "未知二进制类型应保留原始字节：{:?}",
        result.rows[0].values[11]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_sql_returns_error() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let err = driver
        .execute(&config, &Query::new("SELEC * FORM x"))
        .await
        .expect_err("应该报语法错误");

    println!("got expected error: {}", err);
    let msg = format!("{err}");
    // 42601 → "SQL 语法错误"
    assert!(
        msg.contains("语法") || msg.contains("syntax"),
        "错误消息应包含语法错误线索：{msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_returns_friendly_error() {
    let mut config = require_env!();
    config.password = "definitely-wrong-password".to_string();

    let driver = PostgresDriver::new();
    let err = driver
        .test_connection(&config)
        .await
        .expect_err("应该报认证错误");

    println!("got expected auth error: {}", err);
    let msg = format!("{err}");
    // 28P01 → "用户名或密码错误"
    assert!(
        msg.contains("用户名或密码") || msg.contains("password") || msg.contains("authentication"),
        "错误消息应包含认证错误线索：{msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_result_set_keeps_columns() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    let result = driver
        .execute(&config, &Query::new("SELECT 1 AS a, 'x' AS b WHERE 1 = 0"))
        .await
        .expect("execute 失败");

    assert!(result.rows.is_empty(), "WHERE 1=0 应返回空 rows");
    // extract_columns_fallback 经 describe 拿列头
    assert_eq!(result.columns.len(), 2, "空结果集仍应有列定义");
    println!("empty result columns: {:?}", result.columns);
}

#[tokio::test(flavor = "multi_thread")]
async fn dollar_quoted_function_body_treated_as_one_statement() {
    let config = require_env!();
    let driver = PostgresDriver::new();

    // dollar-quoted 函数体内的 ; 不应被切分
    let sql = "DO $$ BEGIN PERFORM 1; PERFORM 2; END; $$; SELECT 99 AS final_value";
    let result = driver
        .execute(&config, &Query::new(sql))
        .await
        .expect("dollar-quoted 多语句执行失败");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0], "final_value");
    println!("dollar-quoted result: {:?}", result.rows[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_dataset_handles_bulk_large_and_native_values() {
    let config = require_env!();
    if !seeded_dataset_enabled() {
        eprintln!("[SKIP] seeded dataset test skipped: RAMAG_TEST_DATASET != full");
        return;
    }
    let driver = PostgresDriver::new();

    let matrix = driver
        .execute(
            &config,
            &Query::new("SELECT * FROM public.type_matrix ORDER BY id"),
        )
        .await
        .expect("type_matrix 查询失败");
    assert_eq!(matrix.rows.len(), 3);

    let page = driver
        .execute(
            &config,
            &Query::new(
                "SELECT id, group_id, status, amount, title, payload, tags, binary_token, created_at \
                 FROM public.bulk_records ORDER BY id LIMIT 5000",
            ),
        )
        .await
        .expect("bulk_records 大分页查询失败");
    assert_eq!(page.rows.len(), 5000);

    let large = driver
        .execute(
            &config,
            &Query::new("SELECT text_value, bytea_value FROM public.large_values WHERE id = 1"),
        )
        .await
        .expect("large_values 查询失败");
    assert!(matches!(&large.rows[0].values[0], Value::Text(value) if value.len() > 1_000_000));
    assert!(matches!(&large.rows[0].values[1], Value::Bytes(value) if value.len() == 1_048_576));
}
