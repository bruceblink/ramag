//! Integration coverage for reviewed schema migration replay on PostgreSQL.

#![allow(clippy::expect_used, clippy::panic)]

use ramag_domain::entities::{ConnectionConfig, DriverKind, Query};
use ramag_domain::traits::Driver;
use ramag_infra_postgres::PostgresDriver;

fn config_from_env() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_PG_HOST").ok()?;
    let port = std::env::var("RAMAG_TEST_PG_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_PG_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_PG_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_PG_DB").ok()?;
    Some(ConnectionConfig {
        driver: DriverKind::Postgres,
        password,
        database: Some(database),
        ..ConnectionConfig::new_mysql("migration-replay", host, port, user)
    })
}

/// Replays a reviewed multi-statement migration in one PostgreSQL transaction.
#[tokio::test(flavor = "multi_thread")]
async fn reviewed_migration_script_replays_on_postgres() {
    let Some(config) = config_from_env() else {
        eprintln!("[SKIP] migration replay skipped: 设置 RAMAG_TEST_PG_* 环境变量后运行");
        return;
    };
    let driver = PostgresDriver::new();
    let suffix = format!("{}_{}", std::process::id(), unique_suffix());
    let source_schema = format!("ramag_migration_source_schema_{suffix}");
    let target_schema = "public";
    let source = format!("ramag_migration_source_{suffix}");
    let target = format!("ramag_migration_target_{suffix}");
    let source_primary = format!("ramag_source_pkey_{suffix}");
    let source_index = format!("ramag_source_idx_name_{suffix}");
    let target_primary = format!("ramag_target_pkey_{suffix}");
    let target_index = format!("ramag_target_idx_legacy_{suffix}");
    let parent = format!("ramag_migration_parent_{suffix}");
    let qualified_source = format!("\"{source_schema}\".\"{source}\"");
    let qualified_target = format!("\"{target_schema}\".\"{target}\"");
    let qualified_parent = format!("\"{target_schema}\".\"{parent}\"");

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {qualified_target} CASCADE; DROP TABLE IF EXISTS {qualified_parent} CASCADE; DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; CREATE SCHEMA \"{source_schema}\"; \
                 CREATE TABLE {qualified_parent} (\"id\" integer NOT NULL PRIMARY KEY); \
                 CREATE TABLE {qualified_source} (\"id\" integer NOT NULL, \"parent_id\" integer, \"name\" text NOT NULL, \
                 CONSTRAINT \"{source_primary}\" PRIMARY KEY (\"id\")); \
                 CREATE INDEX \"{source_index}\" ON {qualified_source} (\"name\"); \
                 CREATE TABLE {qualified_target} (\"id\" bigint NULL, \"parent_id\" integer, \"legacy\" text, \
                 CONSTRAINT \"{target_primary}\" PRIMARY KEY (\"id\"), \
                 CONSTRAINT \"fk_migration_replay_parent\" FOREIGN KEY (\"parent_id\") REFERENCES {qualified_parent} (\"id\") \
                 ON DELETE RESTRICT ON UPDATE RESTRICT); \
                 CREATE INDEX \"{target_index}\" ON {qualified_target} (\"legacy\")"
            )),
        )
        .await
        .expect("创建 PostgreSQL 迁移回放测试表失败");

    let migration_sql = format!(
        "-- Ramag migration preview\n\
         ALTER TABLE {qualified_target} DROP CONSTRAINT \"fk_migration_replay_parent\";\n\
         DROP INDEX \"{target_schema}\".\"{target_index}\";\n\
         ALTER TABLE {qualified_target} DROP CONSTRAINT \"{target_primary}\";\n\
         ALTER TABLE {qualified_target} DROP COLUMN \"legacy\";\n\
         ALTER TABLE {qualified_target} ALTER COLUMN \"id\" TYPE integer;\n\
         ALTER TABLE {qualified_target} ALTER COLUMN \"id\" SET NOT NULL;\n\
         ALTER TABLE {qualified_target} ADD COLUMN \"name\" text NOT NULL;\n\
         CREATE INDEX \"{source_index}\" ON {qualified_target} (\"name\");\n\
         ALTER TABLE {qualified_target} ADD CONSTRAINT \"{source_primary}\" PRIMARY KEY (\"id\");\n\
         ALTER TABLE {qualified_target} ADD CONSTRAINT \"fk_migration_replay_parent\" FOREIGN KEY (\"parent_id\") \
         REFERENCES {qualified_parent} (\"id\") ON DELETE CASCADE ON UPDATE CASCADE;"
    );
    driver
        .execute(
            &config,
            &Query::new(migration_sql)
                .with_schema(target_schema)
                .transactional(),
        )
        .await
        .expect("PostgreSQL 迁移回放失败");

    let columns = driver
        .list_columns(&config, target_schema, &target)
        .await
        .expect("读取 PostgreSQL 迁移回放列失败");
    assert!(columns.iter().any(|column| column.name == "id"));
    assert!(columns.iter().any(|column| column.name == "name"));
    assert!(!columns.iter().any(|column| column.name == "legacy"));

    let indexes = driver
        .list_indexes(&config, target_schema, &target)
        .await
        .expect("读取 PostgreSQL 迁移回放索引失败");
    assert!(indexes.iter().any(|index| index.name == source_primary));
    assert!(indexes.iter().any(|index| index.name == source_index));
    assert!(!indexes.iter().any(|index| index.name == target_index));
    assert!(!indexes.iter().any(|index| index.name == target_primary));

    let foreign_keys = driver
        .list_foreign_keys(&config, target_schema, &target)
        .await
        .expect("读取 PostgreSQL 迁移回放外键失败");
    assert!(foreign_keys.iter().any(|foreign_key| {
        foreign_key.name == "fk_migration_replay_parent"
            && foreign_key.on_delete == ramag_domain::entities::ForeignKeyAction::Cascade
            && foreign_key.on_update == ramag_domain::entities::ForeignKeyAction::Cascade
    }));

    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {qualified_target} CASCADE; DROP TABLE IF EXISTS {qualified_parent} CASCADE; DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE;"
            )),
        )
        .await
        .expect("清理 PostgreSQL 迁移回放测试表失败");
}

/// Confirms that a failed reviewed migration does not leave a partial PostgreSQL schema.
#[tokio::test(flavor = "multi_thread")]
async fn failed_migration_rolls_back_on_postgres() {
    let Some(config) = config_from_env() else {
        eprintln!("[SKIP] migration rollback skipped: 设置 RAMAG_TEST_PG_* 环境变量后运行");
        return;
    };
    let driver = PostgresDriver::new();
    let table = format!(
        "ramag_migration_rollback_{}_{}",
        std::process::id(),
        unique_suffix()
    );
    let qualified = format!("\"public\".\"{table}\"");
    driver
        .execute(
            &config,
            &Query::new(format!(
                "DROP TABLE IF EXISTS {qualified}; CREATE TABLE {qualified} (\"legacy\" integer NOT NULL)"
            )),
        )
        .await
        .expect("创建 PostgreSQL 迁移回滚测试表失败");

    let result = driver
        .execute(
            &config,
            &Query::new(format!(
                "ALTER TABLE {qualified} DROP COLUMN \"legacy\"; ALTER TABLE {qualified} ADD COLUMN \"broken\" missing_type"
            ))
            .with_schema("public")
            .transactional(),
        )
        .await;
    assert!(result.is_err(), "故意失败的 PostgreSQL 迁移应返回错误");

    let columns = driver
        .list_columns(&config, "public", &table)
        .await
        .expect("读取 PostgreSQL 迁移回滚结果失败");
    assert!(
        columns.iter().any(|column| column.name == "legacy"),
        "事务回滚后旧列必须保留"
    );

    driver
        .execute(
            &config,
            &Query::new(format!("DROP TABLE IF EXISTS {qualified};")),
        )
        .await
        .expect("清理 PostgreSQL 迁移回滚测试表失败");
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix epoch")
        .as_nanos()
}
