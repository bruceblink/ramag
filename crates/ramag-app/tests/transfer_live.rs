#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! 按库导出 / 导入端到端集成测试：连真实四库容器（与 infra 集成测试同一套
//! RAMAG_TEST_* 环境变量，缺变量软跳过，`make test` 恒绿）。
//!
//! 流程统一为：建临时源库 → 导出文件 → 删源库 → 导入重建 → 校验数据保真与
//! 序列续值 → 重复导入验证幂等 → 清理。
//! 跑法：`make db-test-up` 起容器后，按 scripts/db-test 的凭据 export
//! `RAMAG_TEST_{MYSQL,PG,REDIS,MONGO}_*`，再 `cargo test -p ramag-app --test transfer_live`。
//! 注意：MySQL 用例要建 / 删临时库，账号需具备 CREATE/DROP DATABASE 权限
//! （dev 容器用 root + RAMAG_DB_TEST_MYSQL_ROOT_PASSWORD）

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ramag_app::usecases::transfer;
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{
    ConflictPolicy, ConnectionConfig, ConnectionId, DriverKind, MongoQuerySpec, Query, QueryRecord,
    QueryRecordId, RedisValue, StreamEntry, TransferProgress, ValuePageCursor,
};
use ramag_domain::error::Result;
use ramag_domain::traits::{Driver, Storage};
use serde_json::{Value, json};

/// 传输编排不触存储；史/偏好走空实现
struct StubStorage;

#[async_trait::async_trait]
impl Storage for StubStorage {
    async fn list_connections(&self) -> Result<Vec<ConnectionConfig>> {
        Ok(Vec::new())
    }
    async fn get_connection(&self, _id: &ConnectionId) -> Result<Option<ConnectionConfig>> {
        Ok(None)
    }
    async fn save_connection(&self, _config: &ConnectionConfig) -> Result<()> {
        Ok(())
    }
    async fn delete_connection(&self, _id: &ConnectionId) -> Result<()> {
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

fn sql_service() -> Arc<ConnectionService> {
    let mut drivers: HashMap<DriverKind, Arc<dyn Driver>> = HashMap::new();
    drivers.insert(
        DriverKind::Mysql,
        Arc::new(ramag_infra_mysql::MysqlDriver::new()),
    );
    drivers.insert(
        DriverKind::Postgres,
        Arc::new(ramag_infra_postgres::PostgresDriver::new()),
    );
    Arc::new(ConnectionService::new(drivers, Arc::new(StubStorage)))
}

fn mysql_config() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_MYSQL_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_MYSQL_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_MYSQL_ADMIN_USER")
        .or_else(|_| std::env::var("RAMAG_TEST_MYSQL_USER"))
        .ok()?;
    let password = std::env::var("RAMAG_TEST_MYSQL_ADMIN_PASSWORD")
        .or_else(|_| std::env::var("RAMAG_TEST_MYSQL_PASSWORD"))
        .ok()?;
    Some(ConnectionConfig {
        password,
        database: std::env::var("RAMAG_TEST_MYSQL_DB").ok(),
        ..ConnectionConfig::new_mysql("transfer-e2e", host, port, user)
    })
}

fn pg_config() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_PG_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_PG_PORT").ok()?.parse().ok()?;
    let user = std::env::var("RAMAG_TEST_PG_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_PG_PASSWORD").ok()?;
    let database = std::env::var("RAMAG_TEST_PG_DB").ok()?;
    Some(ConnectionConfig {
        driver: DriverKind::Postgres,
        password,
        database: Some(database),
        ..ConnectionConfig::new_mysql("transfer-e2e", host, port, user)
    })
}

fn redis_config() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_REDIS_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_REDIS_PORT").ok()?.parse().ok()?;
    let password = std::env::var("RAMAG_TEST_REDIS_PASSWORD").unwrap_or_default();
    Some(ConnectionConfig {
        password,
        ..ConnectionConfig::new_redis("transfer-e2e", host, port)
    })
}

fn mongo_config() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_MONGO_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_MONGO_PORT").ok()?.parse().ok()?;
    let mut cfg = ConnectionConfig::new_mongodb("transfer-e2e", host, port);
    if let Ok(user) = std::env::var("RAMAG_TEST_MONGO_USER") {
        cfg.username = user;
    }
    if let Ok(password) = std::env::var("RAMAG_TEST_MONGO_PASSWORD") {
        cfg.password = password;
    }
    cfg.database = std::env::var("RAMAG_TEST_MONGO_DB").ok();
    Some(cfg)
}

fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ramag-transfer-e2e-{}-{name}", std::process::id()))
}

fn noop_progress() -> impl Fn(TransferProgress) + Send + Sync {
    |_| {}
}

async fn exec(svc: &ConnectionService, config: &ConnectionConfig, sql: impl Into<String>) {
    let sql = sql.into();
    svc.execute(config, &Query::new(sql.clone()))
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\nSQL: {sql}"));
}

async fn scalar_i64(svc: &ConnectionService, config: &ConnectionConfig, sql: &str) -> i64 {
    let result = svc.execute(config, &Query::new(sql)).await.expect(sql);
    match result.rows.first().and_then(|row| row.values.first()) {
        Some(ramag_domain::entities::Value::Int(value)) => *value,
        Some(ramag_domain::entities::Value::Text(text)) => text.parse().expect("数字解析"),
        other => panic!("期望整数标量，实得 {other:?}（SQL: {sql}）"),
    }
}

async fn scalar_value(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    sql: &str,
) -> ramag_domain::entities::Value {
    let result = svc.execute(config, &Query::new(sql)).await.expect(sql);
    result
        .rows
        .first()
        .and_then(|row| row.values.first())
        .cloned()
        .unwrap_or_else(|| panic!("无结果（SQL: {sql}）"))
}

async fn flush_db(svc: &RedisService, config: &ConnectionConfig, db: u8) {
    let _ = svc
        .execute_command(config, db, vec!["FLUSHDB".into()])
        .await;
}

#[path = "transfer_live/database_roundtrip_tests.rs"]
mod database_roundtrip_tests;
#[path = "transfer_live/jsonl_tests.rs"]
mod jsonl_tests;
#[path = "transfer_live/performance_tests.rs"]
mod performance_tests;
#[path = "transfer_live/sql_roundtrip_tests.rs"]
mod sql_roundtrip_tests;
