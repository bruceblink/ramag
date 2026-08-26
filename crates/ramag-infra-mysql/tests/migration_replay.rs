//! Integration coverage for replaying a reviewed schema migration script on MySQL.

#![allow(clippy::expect_used, clippy::panic)]

use ramag_domain::entities::{ConnectionConfig, Query};
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
        ..ConnectionConfig::new_mysql("migration-replay", host, port, user)
    })
}

/// Replays the same bounded multi-statement shape used by the migration UI and checks metadata.
#[tokio::test(flavor = "multi_thread")]
async fn reviewed_migration_script_replays_on_mysql() {
    let Some(config) = config_from_env() else {
        eprintln!("[SKIP] migration replay skipped: 设置 RAMAG_TEST_MYSQL_* 环境变量后运行");
        return;
    };
    let driver = MysqlDriver::new();
    let schema = config
        .database
        .clone()
        .unwrap_or_else(|| "ramag_test".into());
    let suffix = format!("{}_{}", std::process::id(), unique_suffix());
    let source = format!("ramag_migration_source_{suffix}");
    let target = format!("ramag_migration_target_{suffix}");
    let quoted_source = format!("`{source}`");
    let quoted_target = format!("`{target}`");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {quoted_target}; DROP TABLE IF EXISTS {quoted_source}; \
                 CREATE TABLE {quoted_source} (id INT NOT NULL, name VARCHAR(255) NOT NULL, \
                 PRIMARY KEY (id), INDEX idx_name (name)); \
                 CREATE TABLE {quoted_target} (id BIGINT NOT NULL, legacy VARCHAR(255), \
                 PRIMARY KEY (id), INDEX idx_legacy (legacy))"
            )),
        )
        .await
        .expect("创建 MySQL 迁移回放测试表失败");

    let migration_sql = format!(
        "-- Ramag migration preview\n\
         ALTER TABLE {quoted_target} DROP INDEX `idx_legacy`;\n\
         ALTER TABLE {quoted_target} DROP PRIMARY KEY;\n\
         ALTER TABLE {quoted_target} DROP COLUMN `legacy`;\n\
         ALTER TABLE {quoted_target} CHANGE COLUMN `id` `id` INT NOT NULL;\n\
         ALTER TABLE {quoted_target} ADD COLUMN `name` VARCHAR(255) NOT NULL;\n\
         ALTER TABLE {quoted_target} ADD INDEX `idx_name` (`name`);\n\
         ALTER TABLE {quoted_target} ADD PRIMARY KEY (`id`);"
    );
    driver
        .execute(&config, &Query::new(migration_sql).with_schema(&schema))
        .await
        .expect("MySQL 迁移回放失败");

    let columns = driver
        .list_columns(&config, &schema, &target)
        .await
        .expect("读取 MySQL 迁移回放列失败");
    assert!(columns.iter().any(|column| column.name == "id"));
    assert!(columns.iter().any(|column| column.name == "name"));
    assert!(!columns.iter().any(|column| column.name == "legacy"));

    let indexes = driver
        .list_indexes(&config, &schema, &target)
        .await
        .expect("读取 MySQL 迁移回放索引失败");
    assert!(indexes.iter().any(|index| index.name == "PRIMARY"));
    assert!(indexes.iter().any(|index| index.name == "idx_name"));
    assert!(!indexes.iter().any(|index| index.name == "idx_legacy"));

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {quoted_target}; DROP TABLE IF EXISTS {quoted_source};"
            )),
        )
        .await
        .expect("清理 MySQL 迁移回放测试表失败");
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix epoch")
        .as_nanos()
}
