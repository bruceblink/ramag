//! 集成测试：连接真实 MySQL。缺 RAMAG_TEST_MYSQL_* 环境变量时跳过

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ramag_domain::entities::{ConnectionConfig, Query, Value};
use ramag_domain::traits::Driver;
use ramag_infra_mysql::MysqlDriver;

/// 缺任一字段就跳过测试
fn config_from_env() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_MYSQL_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_MYSQL_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_MYSQL_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_MYSQL_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_MYSQL_DB").ok();

    Some(ConnectionConfig {
        password,
        database,
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
                    "[SKIP] integration test skipped: 设置 RAMAG_TEST_MYSQL_* 环境变量后运行"
                );
                return;
            }
        }
    }};
}

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_works() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    driver
        .test_connection(&config)
        .await
        .expect("test_connection 失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_schemas_returns_data() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let schemas = driver
        .list_schemas(&config)
        .await
        .expect("list_schemas 失败");
    println!("schemas: {:#?}", schemas);
    assert!(!schemas.is_empty(), "至少应返回一个 schema");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_tables_for_db() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let schema = config
        .database
        .clone()
        .unwrap_or_else(|| "midas_storage".into());
    let tables = driver
        .list_tables(&config, &schema)
        .await
        .expect("list_tables 失败");
    println!("tables in {}: {:#?}", schema, tables);
    // 不强制有表，只验证调用成功
}

#[tokio::test(flavor = "multi_thread")]
async fn list_columns_for_first_table() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let schema = config
        .database
        .clone()
        .unwrap_or_else(|| "midas_storage".into());

    let tables = driver
        .list_tables(&config, &schema)
        .await
        .expect("list_tables 失败");

    if let Some(first_table) = tables.first() {
        let columns = driver
            .list_columns(&config, &schema, &first_table.name)
            .await
            .expect("list_columns 失败");
        println!("columns of {}.{}: {:#?}", schema, first_table.name, columns);
        assert!(!columns.is_empty(), "表应至少有一列");
    } else {
        eprintln!("[INFO] 库 {} 没有表，跳过列检查", schema);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_select_one() {
    let config = require_env!();
    let driver = MysqlDriver::new();

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

/// Verifies that the MySQL driver exposes EXPLAIN output as a normal result set.
#[tokio::test(flavor = "multi_thread")]
async fn explain_select_returns_plan_rows() {
    let config = require_env!();
    let driver = MysqlDriver::new();

    let result = driver
        .execute(&config, &Query::new("EXPLAIN SELECT 1 AS one"))
        .await
        .expect("EXPLAIN 执行失败");

    assert!(!result.columns.is_empty(), "EXPLAIN 应返回计划列");
    assert!(!result.rows.is_empty(), "EXPLAIN 应返回至少一行计划");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_select_with_types() {
    let config = require_env!();
    let driver = MysqlDriver::new();

    let result = driver
        .execute(
            &config,
            &Query::new(
                "SELECT \
                    1 AS i, \
                    1.5 AS f, \
                    'text' AS t, \
                    NULL AS n, \
                    NOW() AS dt, \
                    JSON_OBJECT('k', 'v') AS j, \
                    b'10101010' AS bits",
            ),
        )
        .await
        .expect("execute 失败");

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.columns.len(), 7);
    assert!(
        matches!(&result.rows[0].values[1], Value::Text(value) if value == "1.5"),
        "DECIMAL 应精确解码为文本，实际：{:?}",
        result.rows[0].values[1]
    );
    assert!(
        matches!(&result.rows[0].values[6], Value::Bytes(value) if value == &[0xAA]),
        "BIT 应保留原始位字节，实际：{:?}",
        result.rows[0].values[6]
    );
    println!("typed result: {:#?}", result.rows[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_sql_returns_error() {
    let config = require_env!();
    let driver = MysqlDriver::new();

    let err = driver
        .execute(&config, &Query::new("SELEC * FORM x"))
        .await
        .expect_err("应该报语法错误");

    println!("got expected error: {}", err);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_returns_friendly_error() {
    let mut config = require_env!();
    config.password = "definitely-wrong-password".to_string();

    let driver = MysqlDriver::new();
    let err = driver
        .test_connection(&config)
        .await
        .expect_err("应该报认证错误");

    println!("got expected auth error: {}", err);
    let msg = format!("{err}");
    assert!(
        msg.contains("用户名或密码"),
        "错误消息应明确说明认证失败，实际：{msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inaccessible_database_returns_friendly_error() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let database = format!("ramag_sync_access_denied_probe_{}", std::process::id());
    let err = driver
        .execute(&config, &Query::new("SELECT 1").with_schema(database))
        .await
        .expect_err("不存在或无权访问的数据库应被拒绝");
    let message = err.to_string();

    // 管理员测试账号会得到 1049；受限账号会得到本测试重点覆盖的 1044。
    if message.contains("数据库不存在") {
        eprintln!("[SKIP] 当前 MySQL 测试账号可访问任意数据库，无法触发 1044");
        return;
    }
    assert!(
        message.contains("目标账号没有创建或访问该数据库的权限"),
        "1044 应转换为可操作的中文权限提示，实际：{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_dataset_handles_bulk_large_and_spatial_values() {
    let config = require_env!();
    if !seeded_dataset_enabled() {
        eprintln!("[SKIP] seeded dataset test skipped: RAMAG_TEST_DATASET != full");
        return;
    }
    let driver = MysqlDriver::new();

    let matrix = driver
        .execute(
            &config,
            &Query::new("SELECT * FROM type_matrix ORDER BY id"),
        )
        .await
        .expect("type_matrix 查询失败");
    assert_eq!(matrix.rows.len(), 3);

    let page = driver
        .execute(
            &config,
            &Query::new(
                "SELECT id, group_id, status, amount, title, payload, binary_token, created_at \
                 FROM bulk_records ORDER BY id LIMIT 5000",
            ),
        )
        .await
        .expect("bulk_records 大分页查询失败");
    assert_eq!(page.rows.len(), 5000);

    let large = driver
        .execute(
            &config,
            &Query::new("SELECT text_value, blob_value FROM large_values WHERE id = 1"),
        )
        .await
        .expect("large_values 查询失败");
    assert!(matches!(&large.rows[0].values[0], Value::Text(value) if value.len() > 1_000_000));
    assert!(matches!(&large.rows[0].values[1], Value::Bytes(value) if value.len() == 1_048_576));

    let spatial = driver
        .execute(
            &config,
            &Query::new("SELECT location, area FROM spatial_samples WHERE id = 1"),
        )
        .await
        .expect("spatial_samples 查询失败");
    assert_eq!(spatial.rows.len(), 1);
    assert!(
        spatial.rows[0]
            .values
            .iter()
            .all(|value| matches!(value, Value::Bytes(bytes) if !bytes.is_empty()))
    );
}
