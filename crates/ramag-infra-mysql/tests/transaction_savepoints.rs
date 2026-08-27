//! MySQL 事务保存点集成测试；缺少 RAMAG_TEST_MYSQL_* 时跳过。

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use ramag_domain::entities::{ConnectionConfig, Query, Value};
use ramag_domain::traits::Driver;
use ramag_infra_mysql::MysqlDriver;

fn config_from_env() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_MYSQL_HOST").ok()?;
    let port = std::env::var("RAMAG_TEST_MYSQL_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_MYSQL_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_MYSQL_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_MYSQL_DB").ok();
    Some(ConnectionConfig {
        password,
        database,
        ..ConnectionConfig::new_mysql("savepoint-test", host, port, user)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn transaction_savepoint_rolls_back_and_releases_without_closing_transaction() {
    let Some(config) = config_from_env() else {
        eprintln!("[SKIP] 保存点集成测试跳过：设置 RAMAG_TEST_MYSQL_* 环境变量后运行");
        return;
    };
    let driver = MysqlDriver::new();
    let table = format!("ramag_savepoint_probe_{}", std::process::id());
    let quoted_table = format!("`{table}`");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {quoted_table}; CREATE TABLE {quoted_table} (id INT PRIMARY KEY, value INT NOT NULL)"
            )),
        )
        .await
        .expect("创建 MySQL 保存点测试表失败");

    let transaction = driver
        .begin_transaction(&config)
        .await
        .expect("开启 MySQL 保存点事务失败");
    driver
        .execute_in_transaction(
            &config,
            &transaction,
            &Query::new(format!("INSERT INTO {quoted_table} VALUES (1, 10)")),
        )
        .await
        .expect("MySQL 保存点前插入失败");
    driver
        .create_savepoint(&config, &transaction, "ramag_sp_before_second")
        .await
        .expect("创建 MySQL 保存点失败");
    driver
        .execute_in_transaction(
            &config,
            &transaction,
            &Query::new(format!("INSERT INTO {quoted_table} VALUES (2, 20)")),
        )
        .await
        .expect("MySQL 保存点后插入失败");
    driver
        .rollback_to_savepoint(&config, &transaction, "ramag_sp_before_second")
        .await
        .expect("回滚到 MySQL 保存点失败");
    driver
        .release_savepoint(&config, &transaction, "ramag_sp_before_second")
        .await
        .expect("释放 MySQL 保存点失败");
    driver
        .execute_in_transaction(
            &config,
            &transaction,
            &Query::new(format!("INSERT INTO {quoted_table} VALUES (3, 30)")),
        )
        .await
        .expect("释放 MySQL 保存点后事务未继续执行");
    driver
        .commit_transaction(&config, &transaction)
        .await
        .expect("提交 MySQL 保存点事务失败");

    let result = driver
        .execute(
            &config,
            &Query::new(format!("SELECT id, value FROM {quoted_table} ORDER BY id")),
        )
        .await
        .expect("读取 MySQL 保存点最终结果失败");
    assert_eq!(result.rows.len(), 2);
    assert!(matches!(result.rows[0].values.first(), Some(Value::Int(1))));
    assert!(matches!(result.rows[1].values.first(), Some(Value::Int(3))));
    driver
        .execute(
            &config,
            &Query::new(format!("DROP TABLE IF EXISTS {quoted_table}")),
        )
        .await
        .expect("清理 MySQL 保存点测试表失败");
}
