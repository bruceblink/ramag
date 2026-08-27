//! 集成测试：连接真实 MySQL。缺 RAMAG_TEST_MYSQL_* 环境变量时跳过

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramag_domain::entities::{ConnectionConfig, ForeignKeyAction, Query, Value};
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

/// 验证结构对比依赖的列、索引和外键元数据能从真实 MySQL 读取。
#[tokio::test(flavor = "multi_thread")]
async fn list_schema_metadata_for_comparison_tables() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let schema = config
        .database
        .clone()
        .unwrap_or_else(|| "midas_storage".into());
    let suffix = std::process::id();
    let source = format!("ramag_metadata_source_{suffix}");
    let target = format!("ramag_metadata_target_{suffix}");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS `{target}`; DROP TABLE IF EXISTS `{source}`; \
                 CREATE TABLE `{source}` (id INT NOT NULL PRIMARY KEY, name VARCHAR(64) NOT NULL, \
                 INDEX idx_metadata_source_name (name))"
            )),
        )
        .await
        .expect("创建 MySQL 元数据源表失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "CREATE TABLE `{target}` (id INT NOT NULL PRIMARY KEY, name VARCHAR(128) NOT NULL, \
                 email VARCHAR(128), INDEX idx_metadata_target_email (email), \
                 CONSTRAINT fk_metadata_target_source FOREIGN KEY (id) REFERENCES `{source}` (id) \
                 ON DELETE CASCADE ON UPDATE CASCADE)"
            )),
        )
        .await
        .expect("创建 MySQL 元数据目标表失败");

    let source_columns = driver
        .list_columns(&config, &schema, &source)
        .await
        .expect("读取 MySQL 源表列失败");
    let target_indexes = driver
        .list_indexes(&config, &schema, &target)
        .await
        .expect("读取 MySQL 目标表索引失败");
    let target_foreign_keys = driver
        .list_foreign_keys(&config, &schema, &target)
        .await
        .expect("读取 MySQL 目标表外键失败");

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
                "DROP TABLE IF EXISTS `{target}`; DROP TABLE IF EXISTS `{source}`;"
            )),
        )
        .await
        .expect("清理 MySQL 元数据测试表失败");
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

#[tokio::test(flavor = "multi_thread")]
async fn execute_write_is_visible_to_an_independent_connection() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let observer = MysqlDriver::new();
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix epoch")
        .as_nanos();
    let table = format!(
        "ramag_autocommit_result_edit_{}_{}",
        std::process::id(),
        suffix
    );
    let quoted_table = format!("`{table}`");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "CREATE TABLE {quoted_table} (id BIGINT PRIMARY KEY, value VARCHAR(255) NOT NULL)"
            )),
        )
        .await
        .expect("创建临时测试表失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "INSERT INTO {quoted_table} (id, value) VALUES (1, 'before')"
            )),
        )
        .await
        .expect("写入临时测试行失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "UPDATE {quoted_table} SET value = 'after' WHERE id = 1"
            )),
        )
        .await
        .expect("更新临时测试行失败");

    let result = observer
        .execute(
            &config,
            &Query::new(format!("SELECT value FROM {quoted_table} WHERE id = 1")),
        )
        .await
        .expect("独立连接查询临时测试行失败");
    assert_eq!(result.rows.len(), 1, "更新后的行应对独立连接可见");
    assert!(matches!(
        &result.rows[0].values[0],
        Value::Text(value) if value == "after"
    ));

    driver
        .execute(&config, &Query::new(format!("DROP TABLE {quoted_table}")))
        .await
        .expect("清理临时测试表失败");
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

/// Verifies that a backend ID exposed by a cancellable query can stop that query.
#[tokio::test(flavor = "multi_thread")]
async fn cancellable_query_can_be_stopped_by_backend_id() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let handle = Arc::new(AtomicU64::new(0));
    let query_handle = handle.clone();
    let worker_driver = driver.clone();
    let worker_config = config.clone();
    let mut task = tokio::spawn(async move {
        worker_driver
            .execute_cancellable(
                &worker_config,
                &Query::new("SELECT SLEEP(30)"),
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
            panic!("MySQL 查询未及时暴露后端线程 ID");
        }
    };

    let cancel_result = tokio::time::timeout(
        Duration::from_secs(5),
        driver.cancel_query(&config, backend_id),
    )
    .await;
    assert!(
        cancel_result.is_ok(),
        "MySQL 后端取消请求超时：{backend_id}"
    );
    cancel_result
        .expect("MySQL 后端取消请求超时")
        .expect("MySQL 后端取消请求失败");

    let execution = tokio::time::timeout(Duration::from_secs(10), &mut task)
        .await
        .expect("MySQL 查询取消后仍未结束")
        .expect("MySQL 查询任务异常退出");
    // MySQL may return a scalar interruption marker instead of an error for SLEEP().
    // Completion within the bounded timeout proves that KILL QUERY stopped the work.
    let result = execution.expect("MySQL 被取消的查询应返回可识别的结果");
    assert!(
        result.elapsed_ms < 10_000,
        "MySQL 查询没有在取消后及时结束：{}ms",
        result.elapsed_ms
    );
}

/// 验证手动事务中的写入可在同一会话内读取，并分别支持回滚和提交。
#[tokio::test(flavor = "multi_thread")]
async fn manual_transaction_commit_and_rollback() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let table = format!("ramag_transaction_probe_{}", std::process::id());
    let quoted_table = format!("`{table}`");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {quoted_table}; CREATE TABLE {quoted_table} (id INT PRIMARY KEY, value INT NOT NULL)"
            )),
        )
        .await
        .expect("创建 MySQL 事务测试表失败");

    let rollback_id = driver
        .begin_transaction(&config)
        .await
        .expect("开启 MySQL 回滚事务失败");
    let inserted = driver
        .execute_in_transaction(
            &config,
            &rollback_id,
            &Query::new(format!("INSERT INTO {quoted_table} VALUES (1, 10)")),
        )
        .await
        .expect("MySQL 事务内插入失败");
    assert_eq!(inserted.affected_rows, 1);
    let inside = driver
        .execute_in_transaction(
            &config,
            &rollback_id,
            &Query::new(format!("SELECT value FROM {quoted_table} WHERE id = 1")),
        )
        .await
        .expect("MySQL 事务内读取失败");
    assert!(matches!(
        inside.rows[0].values.first(),
        Some(Value::Int(10))
    ));
    driver
        .rollback_transaction(&config, &rollback_id)
        .await
        .expect("MySQL 回滚事务失败");

    let committed_id = driver
        .begin_transaction(&config)
        .await
        .expect("开启 MySQL 提交事务失败");
    driver
        .execute_in_transaction(
            &config,
            &committed_id,
            &Query::new(format!("INSERT INTO {quoted_table} VALUES (2, 20)")),
        )
        .await
        .expect("MySQL 提交事务内插入失败");
    driver
        .commit_transaction(&config, &committed_id)
        .await
        .expect("MySQL 提交事务失败");

    let outside = driver
        .execute(
            &config,
            &Query::new(format!("SELECT id, value FROM {quoted_table} ORDER BY id")),
        )
        .await
        .expect("读取 MySQL 事务最终结果失败");
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
            &Query::new(format!("DROP TABLE IF EXISTS {quoted_table}")),
        )
        .await
        .expect("清理 MySQL 事务测试表失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_write_is_visible_to_an_independent_connection() {
    let config = require_env!();
    let driver = MysqlDriver::new();
    let observer = MysqlDriver::new();
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix epoch")
        .as_nanos();
    let table = format!(
        "ramag_autocommit_result_edit_{}_{}",
        std::process::id(),
        suffix
    );
    let quoted_table = format!("`{table}`");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "CREATE TABLE {quoted_table} (id BIGINT PRIMARY KEY, value VARCHAR(255) NOT NULL)"
            )),
        )
        .await
        .expect("创建临时测试表失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "INSERT INTO {quoted_table} (id, value) VALUES (1, 'before')"
            )),
        )
        .await
        .expect("写入临时测试行失败");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "UPDATE {quoted_table} SET value = 'after' WHERE id = 1"
            )),
        )
        .await
        .expect("更新临时测试行失败");

    let result = observer
        .execute(
            &config,
            &Query::new(format!("SELECT value FROM {quoted_table} WHERE id = 1")),
        )
        .await
        .expect("独立连接查询临时测试行失败");
    assert_eq!(result.rows.len(), 1, "更新后的行应对独立连接可见");
    assert!(matches!(
        &result.rows[0].values[0],
        Value::Text(value) if value == "after"
    ));

    driver
        .execute(&config, &Query::new(format!("DROP TABLE {quoted_table}")))
        .await
        .expect("清理临时测试表失败");
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
