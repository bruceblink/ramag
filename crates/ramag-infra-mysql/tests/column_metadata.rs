//! 集成验证 MySQL 列顺序、AUTO_INCREMENT 和生成列元数据。

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use ramag_domain::entities::{ConnectionConfig, GeneratedColumnStorage, Query};
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
        ..ConnectionConfig::new_mysql("column-metadata-test", host, port, user)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn list_columns_preserves_mysql_generation_metadata() {
    let Some(config) = config_from_env() else {
        eprintln!("[SKIP] integration test skipped: 设置 RAMAG_TEST_MYSQL_* 环境变量后运行");
        return;
    };
    let driver = MysqlDriver::new();
    let schema = config
        .database
        .clone()
        .unwrap_or_else(|| "midas_storage".into());
    let table = format!("ramag_column_metadata_{}", std::process::id());
    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS `{table}`; CREATE TABLE `{table}` (\
                 id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,\
                 price INT NOT NULL,\
                 total INT GENERATED ALWAYS AS (price + 1) STORED,\
                 virtual_total INT GENERATED ALWAYS AS (price + 2) VIRTUAL)"
            )),
        )
        .await
        .expect("创建 MySQL 生成列测试表失败");

    let columns = driver
        .list_columns(&config, &schema, &table)
        .await
        .expect("读取 MySQL 生成列测试表失败");
    assert_eq!(
        columns
            .iter()
            .map(|column| column.ordinal_position)
            .collect::<Vec<_>>(),
        [Some(1), Some(2), Some(3), Some(4)]
    );
    let id = columns
        .iter()
        .find(|column| column.name == "id")
        .expect("id");
    assert!(id.is_auto_increment);
    let total = columns
        .iter()
        .find(|column| column.name == "total")
        .expect("total");
    assert_eq!(
        total.generated_storage,
        Some(GeneratedColumnStorage::Stored)
    );
    assert!(
        total
            .generation_expression
            .as_deref()
            .is_some_and(|expression| expression.contains("price + 1"))
    );
    let virtual_total = columns
        .iter()
        .find(|column| column.name == "virtual_total")
        .expect("virtual_total");
    assert_eq!(
        virtual_total.generated_storage,
        Some(GeneratedColumnStorage::Virtual)
    );

    driver
        .execute(
            &config,
            &Query::new(format!("DROP TABLE IF EXISTS `{table}`;")),
        )
        .await
        .expect("清理 MySQL 生成列测试表失败");
}
