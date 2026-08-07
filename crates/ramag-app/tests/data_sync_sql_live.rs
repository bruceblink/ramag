#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! SQL 数据同步真实库测试。缺对应环境变量时软跳过。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ramag_app::{
    ConnectionService, DataSyncConfirmation, DataSyncGate, DataSyncGatePhase, DataSyncService,
    MongoService,
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

fn postgres_distinct_configs() -> Option<(ConnectionConfig, ConnectionConfig)> {
    let (source, _) = postgres_configs()?;
    let host = std::env::var("RAMAG_TEST_PG_DISTINCT_HOST").ok()?;
    let port = std::env::var("RAMAG_TEST_PG_DISTINCT_PORT")
        .ok()?
        .parse()
        .ok()?;
    let username = std::env::var("RAMAG_TEST_PG_DISTINCT_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_PG_DISTINCT_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_PG_DISTINCT_DB").ok()?;
    let mut target = ConnectionConfig {
        driver: DriverKind::Postgres,
        password,
        database: Some(database),
        ..ConnectionConfig::new_mysql("pg-sync-distinct-target", host, port, username)
    };
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
    let mongo_service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        storage,
    ));
    let gate = Arc::new(DataSyncGate::default());
    let sync_service =
        DataSyncService::new(connection_service.clone(), mongo_service, gate.clone());
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

#[path = "data_sync_sql_live/mysql_tests.rs"]
mod mysql_tests;
#[path = "data_sync_sql_live/postgres_schema_tests.rs"]
mod postgres_schema_tests;
#[path = "data_sync_sql_live/postgres_search_path_tests.rs"]
mod postgres_search_path_tests;
