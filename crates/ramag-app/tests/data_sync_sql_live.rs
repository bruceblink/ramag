#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! SQL 数据同步真实库测试。缺对应环境变量时软跳过。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ramag_app::{
    ConnectionService, DataSyncConfirmation, DataSyncGate, DataSyncGatePhase, DataSyncService,
    MongoService, RedisService,
};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DataSyncRequest, DataSyncScope, DataSyncSummary,
    DataSyncTaskId, DriverKind, Query, QueryRecord, QueryRecordId, SqlSyncScope, SyncObjectMapping,
    SyncObjectSelection, Value,
};
use ramag_domain::error::Result;
use ramag_domain::traits::{Driver, Storage};

struct MemoryStorage {
    connections: RwLock<Vec<ConnectionConfig>>,
}

impl MemoryStorage {
    fn new(connections: Vec<ConnectionConfig>) -> Self {
        Self {
            connections: RwLock::new(connections),
        }
    }
}

#[async_trait::async_trait]
impl Storage for MemoryStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(self.connections.read().expect("读取测试连接锁").clone())
    }

    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(self
            .connections
            .read()
            .expect("读取测试连接锁")
            .iter()
            .find(|config| &config.id == id)
            .cloned())
    }

    async fn save_connection(&self, config: &ConnectionConfig) -> Result<()> {
        let mut connections = self.connections.write().expect("写入测试连接锁");
        if let Some(existing) = connections.iter_mut().find(|item| item.id == config.id) {
            *existing = config.clone();
        } else {
            connections.push(config.clone());
        }
        Ok(())
    }

    async fn delete_connection(&self, id: &ConnectionId) -> Result<()> {
        self.connections
            .write()
            .expect("写入测试连接锁")
            .retain(|config| &config.id != id);
        Ok(())
    }

    async fn append_history(&self, _record: &QueryRecord) -> Result<()> {
        Ok(())
    }

    async fn list_history(
        &self,
        _connection_id: Option<&ConnectionId>,
        _limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        Ok(Vec::new())
    }

    async fn delete_history(&self, _id: &QueryRecordId) -> Result<()> {
        Ok(())
    }

    async fn clear_history(&self, _connection_id: Option<&ConnectionId>) -> Result<()> {
        Ok(())
    }

    async fn get_preference(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_preference(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

fn mysql_configs() -> Option<(ConnectionConfig, ConnectionConfig)> {
    let host = std::env::var("RAMAG_TEST_MYSQL_HOST").ok()?;
    let port = std::env::var("RAMAG_TEST_MYSQL_PORT").ok()?.parse().ok()?;
    let username = std::env::var("RAMAG_TEST_MYSQL_ADMIN_USER")
        .or_else(|_| std::env::var("RAMAG_TEST_MYSQL_USER"))
        .ok()?;
    let password = std::env::var("RAMAG_TEST_MYSQL_ADMIN_PASSWORD")
        .or_else(|_| std::env::var("RAMAG_TEST_MYSQL_PASSWORD"))
        .ok()?;
    let database = std::env::var("RAMAG_TEST_MYSQL_DB").ok();
    let mut source = ConnectionConfig {
        password: password.clone(),
        database: database.clone(),
        ..ConnectionConfig::new_mysql("mysql-sync-source", host.clone(), port, username.clone())
    };
    let mut target = ConnectionConfig {
        password,
        database,
        ..ConnectionConfig::new_mysql("mysql-sync-target", host, port, username)
    };
    source.id = ConnectionId::new();
    target.id = ConnectionId::new();
    Some((source, target))
}

fn postgres_configs() -> Option<(ConnectionConfig, ConnectionConfig)> {
    let host = std::env::var("RAMAG_TEST_PG_HOST").ok()?;
    let port = std::env::var("RAMAG_TEST_PG_PORT").ok()?.parse().ok()?;
    let username = std::env::var("RAMAG_TEST_PG_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_PG_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_PG_DB").ok()?;
    let mut source = ConnectionConfig {
        driver: DriverKind::Postgres,
        password: password.clone(),
        database: Some(database.clone()),
        ..ConnectionConfig::new_mysql("pg-sync-source", host.clone(), port, username.clone())
    };
    let mut target = ConnectionConfig {
        driver: DriverKind::Postgres,
        password,
        database: Some(database),
        ..ConnectionConfig::new_mysql("pg-sync-target", host, port, username)
    };
    source.id = ConnectionId::new();
    target.id = ConnectionId::new();
    Some((source, target))
}

fn services(
    source: &ConnectionConfig,
    target: &ConnectionConfig,
) -> (DataSyncService, Arc<ConnectionService>, Arc<DataSyncGate>) {
    let storage: Arc<dyn Storage> =
        Arc::new(MemoryStorage::new(vec![source.clone(), target.clone()]));
    let mut drivers: HashMap<DriverKind, Arc<dyn Driver>> = HashMap::new();
    drivers.insert(
        DriverKind::Mysql,
        Arc::new(ramag_infra_mysql::MysqlDriver::new()),
    );
    drivers.insert(
        DriverKind::Postgres,
        Arc::new(ramag_infra_postgres::PostgresDriver::new()),
    );
    let connection_service = Arc::new(ConnectionService::new(drivers, storage.clone()));
    let redis_service = Arc::new(RedisService::new(
        Arc::new(ramag_infra_redis::RedisDriver::new()),
        storage.clone(),
    ));
    let mongo_service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        storage,
    ));
    let gate = Arc::new(DataSyncGate::default());
    let sync_service = DataSyncService::new(
        connection_service.clone(),
        redis_service,
        mongo_service,
        gate.clone(),
    );
    (sync_service, connection_service, gate)
}

fn sql_request(
    source: &ConnectionConfig,
    target: &ConnectionConfig,
    source_namespace: &str,
    target_namespace: &str,
    mappings: &[(&str, &str)],
) -> DataSyncRequest {
    DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: source.driver,
        scope: DataSyncScope::Sql(SqlSyncScope {
            source_namespace: source_namespace.into(),
            target_namespace: target_namespace.into(),
            tables: SyncObjectSelection::Selected(
                mappings
                    .iter()
                    .map(|(source, target)| SyncObjectMapping {
                        source: (*source).into(),
                        target: (*target).into(),
                    })
                    .collect(),
            ),
        }),
    }
}

async fn execute_sync(
    service: &DataSyncService,
    gate: &DataSyncGate,
    request: DataSyncRequest,
    confirmation: DataSyncConfirmation,
) -> DataSyncSummary {
    let prepared = service.preflight(request).await.expect("同步预检");
    let started = service.start(prepared, confirmation).expect("同步开始");
    let permit = started.permit().clone();
    service.execute(started).await;
    let snapshot = gate.snapshot().expect("同步结果应保持占屏");
    assert_eq!(
        snapshot.phase,
        DataSyncGatePhase::Completed,
        "{:?}",
        snapshot.error
    );
    let summary = snapshot.summary.expect("同步汇总");
    assert!(service.acknowledge_result(&permit));
    summary
}

async fn exec(service: &ConnectionService, config: &ConnectionConfig, sql: impl Into<String>) {
    let sql = sql.into();
    service
        .execute(config, &Query::new(sql.clone()))
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\nSQL: {sql}"));
}

async fn scalar_i64(
    service: &ConnectionService,
    config: &ConnectionConfig,
    sql: impl Into<String>,
) -> i64 {
    let sql = sql.into();
    let result = service
        .execute(config, &Query::new(sql.clone()))
        .await
        .unwrap_or_else(|error| panic!("查询失败：{error}\nSQL: {sql}"));
    match result.rows.first().and_then(|row| row.values.first()) {
        Some(Value::Int(value)) => *value,
        Some(Value::Text(value)) => value.parse().expect("整数文本"),
        other => panic!("期望整数，实得 {other:?}"),
    }
}

async fn scalar_text(
    service: &ConnectionService,
    config: &ConnectionConfig,
    sql: impl Into<String>,
) -> String {
    let sql = sql.into();
    let result = service
        .execute(config, &Query::new(sql.clone()))
        .await
        .unwrap_or_else(|error| panic!("查询失败：{error}\nSQL: {sql}"));
    match result.rows.first().and_then(|row| row.values.first()) {
        Some(Value::Text(value)) => value.clone(),
        other => panic!("期望文本，实得 {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_sync_maps_database_and_tables_without_overwriting() {
    let Some((source, target)) = mysql_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MYSQL_* 后运行 SQL 同步测试");
        return;
    };
    let suffix = std::process::id();
    let source_db = format!("ramag_sync_mysql_src_{suffix}");
    let target_db = format!("ramag_sync_mysql_dst_{suffix}");
    let new_db = format!("ramag_sync_mysql_new_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    let sync = Arc::new(sync);
    exec(
        &sql,
        &source,
        format!(
            "DROP DATABASE IF EXISTS `{source_db}`; DROP DATABASE IF EXISTS `{target_db}`; DROP DATABASE IF EXISTS `{new_db}`; \
             CREATE DATABASE `{source_db}`; CREATE DATABASE `{target_db}`; \
             CREATE TABLE `{source_db}`.`parent` (`id` INT NOT NULL AUTO_INCREMENT, `email` VARCHAR(64) NOT NULL, `name` VARCHAR(64) NOT NULL, PRIMARY KEY (`id`), UNIQUE KEY `uq_email` (`email`)); \
             CREATE TABLE `{source_db}`.`child` (`id` INT NOT NULL, `parent_id` INT NOT NULL, PRIMARY KEY (`id`), CONSTRAINT `fk_child_parent` FOREIGN KEY (`parent_id`) REFERENCES `{source_db}`.`parent` (`id`)); \
             INSERT INTO `{source_db}`.`parent` (`id`,`email`,`name`) VALUES (1,'one@test','source-one'),(2,'two@test','source-two'); \
             INSERT INTO `{source_db}`.`child` VALUES (10,1),(11,2); \
             CREATE TABLE `{target_db}`.`parent_copy` (`id` INT NOT NULL AUTO_INCREMENT, `email` VARCHAR(64) NOT NULL, `name` VARCHAR(64) NOT NULL, PRIMARY KEY (`id`), UNIQUE KEY `uq_email` (`email`)); \
             INSERT INTO `{target_db}`.`parent_copy` (`id`,`email`,`name`) VALUES (1,'one@test','target-kept');"
        ),
    )
    .await;

    let mysql_scopes = sync
        .list_catalog_scopes(&source)
        .await
        .expect("读取 MySQL Database 目录");
    assert!(mysql_scopes.contains(&source_db));
    assert!(!mysql_scopes.contains(&"information_schema".to_string()));
    let mysql_catalog = sync
        .list_catalog_objects(&source, &source_db)
        .await
        .expect("读取 MySQL Table 目录");
    assert!(!mysql_catalog.truncated);
    assert!(mysql_catalog.names.contains(&"parent".to_string()));
    assert!(mysql_catalog.names.contains(&"child".to_string()));

    let request = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("parent", "parent_copy"), ("child", "child_copy")],
    );
    let prepared = sync.preflight(request.clone()).await.expect("MySQL 预检");
    assert!(prepared.report().requires_second_confirmation);
    assert!(
        sync.start(prepared, DataSyncConfirmation::CreateMissingTargets)
            .is_err()
    );
    let summary = execute_sync(
        &sync,
        &gate,
        request.clone(),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(summary.inserted, 3);
    assert_eq!(summary.skipped, 1);
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!("SELECT `name` FROM `{target_db}`.`parent_copy` WHERE `id`=1;")
        )
        .await,
        "target-kept"
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM `{target_db}`.`child_copy`;")
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_SCHEMA='{target_db}' AND TABLE_NAME='child_copy';")
        )
        .await,
        1
    );

    let repeat = execute_sync(
        &sync,
        &gate,
        request,
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(repeat.inserted, 0);
    assert_eq!(repeat.skipped, 4);

    let create_request = sql_request(
        &source,
        &target,
        &source_db,
        &new_db,
        &[("parent", "parent_archive")],
    );
    let created = execute_sync(
        &sync,
        &gate,
        create_request,
        DataSyncConfirmation::CreateMissingTargets,
    )
    .await;
    assert_eq!(created.inserted, 2);
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM `{new_db}`.`parent_archive`;")
        )
        .await,
        2
    );

    // 5,001 行跨越默认 5,000 行批次；目标两端已有记录，中间缺口必须完整补齐。
    let bulk_values = (1..=5_001)
        .map(|id| format!("({id},'source-{id}')"))
        .collect::<Vec<_>>()
        .join(",");
    exec(
        &sql,
        &source,
        format!(
            "CREATE TABLE `{source_db}`.`bulk` (`id` INT NOT NULL, `payload` VARCHAR(32) NOT NULL, PRIMARY KEY (`id`)); \
             CREATE TABLE `{target_db}`.`bulk_copy` (`id` INT NOT NULL, `payload` VARCHAR(32) NOT NULL, PRIMARY KEY (`id`)); \
             INSERT INTO `{source_db}`.`bulk` VALUES {bulk_values}; \
             INSERT INTO `{target_db}`.`bulk_copy` VALUES (1,'target-kept-1'),(5001,'target-kept-5001');"
        ),
    )
    .await;
    let bulk_request = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("bulk", "bulk_copy")],
    );
    let bulk_summary = execute_sync(
        &sync,
        &gate,
        bulk_request.clone(),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(bulk_summary.scanned, 5_001);
    assert_eq!(bulk_summary.inserted, 4_999);
    assert_eq!(bulk_summary.skipped, 2);
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!("SELECT `payload` FROM `{target_db}`.`bulk_copy` WHERE `id`=1;")
        )
        .await,
        "target-kept-1"
    );
    let bulk_repeat = execute_sync(
        &sync,
        &gate,
        bulk_request,
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(bulk_repeat.inserted, 0);
    assert_eq!(bulk_repeat.skipped, 5_001);

    exec(
        &sql,
        &source,
        format!(
            "DROP DATABASE IF EXISTS `{source_db}`; DROP DATABASE IF EXISTS `{target_db}`; DROP DATABASE IF EXISTS `{new_db}`;"
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_preflight_and_write_conflicts_fail_without_overwriting() {
    let Some((source, target)) = mysql_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MYSQL_* 后运行 SQL 同步边界测试");
        return;
    };
    let suffix = std::process::id();
    let source_db = format!("ramag_sync_mysql_edge_src_{suffix}");
    let target_db = format!("ramag_sync_mysql_edge_dst_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &source,
        format!(
            "DROP DATABASE IF EXISTS `{source_db}`; DROP DATABASE IF EXISTS `{target_db}`; \
             CREATE DATABASE `{source_db}`; CREATE DATABASE `{target_db}`; \
             CREATE TABLE `{source_db}`.`prefix_identity` (`code` VARCHAR(255) NOT NULL, UNIQUE KEY `uq_code` (`code`(10))); \
             CREATE TABLE `{source_db}`.`typed` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL); \
             CREATE TABLE `{target_db}`.`typed_copy` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(16) NOT NULL); \
             CREATE TABLE `{target_db}`.`extra_required` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL, `required_value` INT NOT NULL); \
             CREATE TABLE `{source_db}`.`conflict_source` (`id` INT NOT NULL PRIMARY KEY, `email` VARCHAR(64) NOT NULL, UNIQUE KEY `uq_email` (`email`)); \
             CREATE TABLE `{target_db}`.`conflict_target` (`id` INT NOT NULL PRIMARY KEY, `email` VARCHAR(64) NOT NULL, UNIQUE KEY `uq_email` (`email`)); \
             INSERT INTO `{source_db}`.`conflict_source` VALUES (1,'source-one@test'),(2,'duplicate@test'); \
             INSERT INTO `{target_db}`.`conflict_target` VALUES (1,'target-kept@test'),(99,'duplicate@test'); \
             CREATE TABLE `{source_db}`.`changed_source` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL); \
             CREATE TABLE `{target_db}`.`changed_target` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL); \
             INSERT INTO `{source_db}`.`changed_source` VALUES (1,'source-one'); \
             CREATE TABLE `{source_db}`.`permission_source` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL); \
             CREATE TABLE `{target_db}`.`permission_target` (`id` INT NOT NULL PRIMARY KEY, `payload` VARCHAR(32) NOT NULL); \
             INSERT INTO `{source_db}`.`permission_source` VALUES (1,'source-one');"
        ),
    )
    .await;

    let no_identity = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("prefix_identity", "prefix_identity_copy")],
    );
    assert!(
        sync.preflight(no_identity)
            .await
            .err()
            .expect("前缀唯一索引不能作为完整记录身份")
            .message()
            .contains("没有主键")
    );

    let incompatible_type = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("typed", "typed_copy")],
    );
    assert!(
        sync.preflight(incompatible_type)
            .await
            .err()
            .expect("列类型不一致必须拒绝")
            .message()
            .contains("类型不兼容")
    );

    let extra_required = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("typed", "extra_required")],
    );
    assert!(
        sync.preflight(extra_required)
            .await
            .err()
            .expect("额外非空无默认列必须拒绝")
            .message()
            .contains("非空且无默认值")
    );

    // 记录身份缺失，但其它唯一键冲突时必须失败，不能借“忽略冲突”吞掉问题。
    let conflict_request = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("conflict_source", "conflict_target")],
    );
    let prepared = sync
        .preflight(conflict_request)
        .await
        .expect("唯一键冲突执行前结构仍兼容");
    let started = sync
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("唯一键冲突测试开始");
    let permit = started.permit().clone();
    sync.execute(started).await;
    let snapshot = gate.snapshot().expect("失败结果应保持占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Failed);
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM `{target_db}`.`conflict_target` WHERE `id`=2;")
        )
        .await,
        0
    );
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!("SELECT `email` FROM `{target_db}`.`conflict_target` WHERE `id`=1;")
        )
        .await,
        "target-kept@test"
    );
    assert!(sync.acknowledge_result(&permit));

    // 预检后的结构变化必须在任何数据写入前阻止执行。
    let changed_request = sql_request(
        &source,
        &target,
        &source_db,
        &target_db,
        &[("changed_source", "changed_target")],
    );
    let prepared = sync
        .preflight(changed_request)
        .await
        .expect("结构变化测试预检");
    exec(
        &sql,
        &target,
        format!(
            "ALTER TABLE `{target_db}`.`changed_target` ADD COLUMN `after_preflight` INT NULL;"
        ),
    )
    .await;
    let started = sync
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("结构复核前进入占屏");
    let permit = started.permit().clone();
    sync.execute(started).await;
    assert_eq!(
        gate.snapshot().map(|snapshot| snapshot.phase),
        Some(DataSyncGatePhase::Failed)
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM `{target_db}`.`changed_target`;")
        )
        .await,
        0
    );
    assert!(sync.acknowledge_result(&permit));

    // 目标账号只有 SELECT 权限：预检可通过，但第一批写入必须明确失败且不改目标。
    let limited_user = format!("ramag_sync_ro_{suffix}");
    let limited_password = DataSyncTaskId::new().0.simple().to_string();
    exec(
        &sql,
        &target,
        format!(
            "DROP USER IF EXISTS '{limited_user}'@'%'; \
             CREATE USER '{limited_user}'@'%' IDENTIFIED BY '{limited_password}'; \
             GRANT SELECT ON `{target_db}`.* TO '{limited_user}'@'%';"
        ),
    )
    .await;
    let mut limited_target = target.clone();
    limited_target.id = ConnectionId::new();
    limited_target.name = "mysql-sync-limited-target".into();
    limited_target.username = limited_user.clone();
    limited_target.password = limited_password;
    limited_target.database = Some(target_db.clone());
    let (limited_sync, limited_sql, limited_gate) = services(&source, &limited_target);
    let prepared = limited_sync
        .preflight(sql_request(
            &source,
            &limited_target,
            &source_db,
            &target_db,
            &[("permission_source", "permission_target")],
        ))
        .await
        .expect("只读权限目标的结构预检应成功");
    let started = limited_sync
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("权限错误应在占屏执行期明确返回");
    let permit = started.permit().clone();
    limited_sync.execute(started).await;
    let snapshot = limited_gate.snapshot().expect("权限失败结果应保持占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Failed);
    assert!(snapshot.error.is_some_and(|error| {
        error.contains("权限") || error.to_ascii_lowercase().contains("denied")
    }));
    assert_eq!(
        scalar_i64(
            &limited_sql,
            &limited_target,
            format!("SELECT COUNT(*) FROM `{target_db}`.`permission_target`;")
        )
        .await,
        0
    );
    assert!(limited_sync.acknowledge_result(&permit));
    exec(
        &sql,
        &target,
        format!("DROP USER IF EXISTS '{limited_user}'@'%';"),
    )
    .await;

    exec(
        &sql,
        &source,
        format!("DROP DATABASE IF EXISTS `{source_db}`; DROP DATABASE IF EXISTS `{target_db}`;"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_sync_maps_schema_enum_identity_sequence_and_foreign_key() {
    let Some((source, target)) = postgres_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 后运行 SQL 同步测试");
        return;
    };
    let suffix = std::process::id();
    let source_schema = format!("ramag_sync_pg_src_{suffix}");
    let target_schema = format!("ramag_sync_pg_dst_{suffix}");
    let new_schema = format!("ramag_sync_pg_new_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{new_schema}\" CASCADE; \
             CREATE SCHEMA \"{source_schema}\"; CREATE SCHEMA \"{target_schema}\"; \
             CREATE TYPE \"{source_schema}\".\"status\" AS ENUM ('active','disabled'); \
             CREATE TYPE \"{target_schema}\".\"status\" AS ENUM ('active','disabled'); \
             CREATE TABLE \"{source_schema}\".\"parent\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \"email\" TEXT NOT NULL UNIQUE, \"state\" \"{source_schema}\".\"status\" NOT NULL); \
             CREATE TABLE \"{source_schema}\".\"child\" (\"id\" BIGINT PRIMARY KEY, \"parent_id\" BIGINT NOT NULL REFERENCES \"{source_schema}\".\"parent\"(\"id\")); \
             INSERT INTO \"{source_schema}\".\"parent\" (\"id\",\"email\",\"state\") OVERRIDING SYSTEM VALUE VALUES (1,'one@test','active'),(2,'two@test','disabled'); \
             INSERT INTO \"{source_schema}\".\"child\" VALUES (10,1),(11,2); \
             CREATE TABLE \"{target_schema}\".\"parent_copy\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \"email\" TEXT NOT NULL UNIQUE, \"state\" \"{target_schema}\".\"status\" NOT NULL); \
             INSERT INTO \"{target_schema}\".\"parent_copy\" (\"id\",\"email\",\"state\") OVERRIDING SYSTEM VALUE VALUES (1,'target-kept@test','active');"
        ),
    )
    .await;

    let postgres_scopes = sync
        .list_catalog_scopes(&source)
        .await
        .expect("读取 PostgreSQL Schema 目录");
    assert!(postgres_scopes.contains(&source_schema));
    assert!(!postgres_scopes.contains(&"pg_catalog".to_string()));
    let postgres_catalog = sync
        .list_catalog_objects(&source, &source_schema)
        .await
        .expect("读取 PostgreSQL Table 目录");
    assert!(!postgres_catalog.truncated);
    assert!(postgres_catalog.names.contains(&"parent".to_string()));
    assert!(postgres_catalog.names.contains(&"child".to_string()));

    let request = sql_request(
        &source,
        &target,
        &source_schema,
        &target_schema,
        &[("parent", "parent_copy"), ("child", "child_copy")],
    );
    let summary = execute_sync(
        &sync,
        &gate,
        request.clone(),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(summary.inserted, 3);
    assert_eq!(summary.skipped, 1);
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!("SELECT \"email\" FROM \"{target_schema}\".\"parent_copy\" WHERE \"id\"=1;")
        )
        .await,
        "target-kept@test"
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM \"{target_schema}\".\"child_copy\";")
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM pg_constraint WHERE conrelid='\"{target_schema}\".\"child_copy\"'::regclass AND contype='f';")
        )
        .await,
        1
    );
    exec(
        &sql,
        &target,
        format!("INSERT INTO \"{target_schema}\".\"parent_copy\" (\"email\",\"state\") VALUES ('next@test','active');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"parent_copy\";")
        )
        .await,
        3
    );

    // 目标序列可能因预分配或历史删除领先于当前最大 ID，同步不得把它倒退。
    exec(
        &sql,
        &target,
        format!(
            "SELECT setval(pg_get_serial_sequence('\"{target_schema}\".\"parent_copy\"', 'id'), 1000, false);"
        ),
    )
    .await;

    let repeat = execute_sync(
        &sync,
        &gate,
        request,
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(repeat.inserted, 0);
    assert_eq!(repeat.skipped, 4);
    exec(
        &sql,
        &target,
        format!(
            "INSERT INTO \"{target_schema}\".\"parent_copy\" (\"email\",\"state\") VALUES ('sequence-ahead@test','active');"
        ),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"parent_copy\";")
        )
        .await,
        1000
    );

    let create_request = sql_request(
        &source,
        &target,
        &source_schema,
        &new_schema,
        &[("parent", "parent_archive")],
    );
    let created = execute_sync(
        &sync,
        &gate,
        create_request,
        DataSyncConfirmation::CreateMissingTargets,
    )
    .await;
    assert_eq!(created.inserted, 2);
    assert_eq!(
        scalar_text(
            &sql,
            &target,
            format!(
                "SELECT \"state\"::text FROM \"{new_schema}\".\"parent_archive\" WHERE \"id\"=2;"
            )
        )
        .await,
        "disabled"
    );
    exec(
        &sql,
        &target,
        format!("INSERT INTO \"{new_schema}\".\"parent_archive\" (\"email\",\"state\") VALUES ('new-next@test','active');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{new_schema}\".\"parent_archive\";")
        )
        .await,
        3
    );

    // 已有目标使用自定义序列名时，应推进目标自己的序列，而不是猜测源序列映射名。
    exec(
        &sql,
        &source,
        format!(
            "CREATE TABLE \"{source_schema}\".\"serial_source\" (\"id\" BIGSERIAL PRIMARY KEY, \"name\" TEXT NOT NULL); \
             INSERT INTO \"{source_schema}\".\"serial_source\" (\"id\",\"name\") VALUES (5,'source-five'); \
             CREATE SEQUENCE \"{target_schema}\".\"custom_serial_sequence\"; \
             CREATE TABLE \"{target_schema}\".\"serial_target\" (\"id\" BIGINT DEFAULT nextval('\"{target_schema}\".\"custom_serial_sequence\"'::regclass) PRIMARY KEY, \"name\" TEXT NOT NULL); \
             ALTER SEQUENCE \"{target_schema}\".\"custom_serial_sequence\" OWNED BY \"{target_schema}\".\"serial_target\".\"id\";"
        ),
    )
    .await;
    let custom_sequence = execute_sync(
        &sync,
        &gate,
        sql_request(
            &source,
            &target,
            &source_schema,
            &target_schema,
            &[("serial_source", "serial_target")],
        ),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(custom_sequence.inserted, 1);
    exec(
        &sql,
        &target,
        format!(
            "INSERT INTO \"{target_schema}\".\"serial_target\" (\"name\") VALUES ('target-next');"
        ),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"serial_target\";")
        )
        .await,
        6
    );

    // Identity 模式不同会影响显式值插入，必须在预检阶段拒绝。
    exec(
        &sql,
        &source,
        format!(
            "CREATE TABLE \"{source_schema}\".\"plain_identity\" (\"id\" BIGINT PRIMARY KEY); \
             CREATE TABLE \"{target_schema}\".\"identity_mismatch\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);"
        ),
    )
    .await;
    let identity_error = sync
        .preflight(sql_request(
            &source,
            &target,
            &source_schema,
            &target_schema,
            &[("plain_identity", "identity_mismatch")],
        ))
        .await
        .err()
        .expect("Identity 模式不一致必须拒绝");
    assert!(identity_error.message().contains("Identity 模式"));

    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{new_schema}\" CASCADE;"
        ),
    )
    .await;
}
