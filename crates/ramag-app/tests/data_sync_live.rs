#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! 数据同步真实数据库测试。缺对应 RAMAG_TEST_* 环境变量时软跳过。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ramag_app::{
    ConnectionService, DataSyncConfirmation, DataSyncGate, DataSyncGatePhase, DataSyncService,
    MongoService,
};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DataSyncRequest, DataSyncScope, DataSyncTaskId, DriverKind,
    MongoQuerySpec, MongoSyncScope, QueryRecord, QueryRecordId, SyncObjectMapping,
    SyncObjectSelection,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{Driver, Storage};
use serde_json::{Value, json};

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

fn mongo_configs() -> Option<(ConnectionConfig, ConnectionConfig)> {
    let host = std::env::var("RAMAG_TEST_MONGO_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_MONGO_PORT").ok()?.parse().ok()?;
    let username = std::env::var("RAMAG_TEST_MONGO_USER").unwrap_or_default();
    let password = std::env::var("RAMAG_TEST_MONGO_PASSWORD").unwrap_or_default();
    let auth_source = std::env::var("RAMAG_TEST_MONGO_AUTH_SOURCE")
        .ok()
        .or_else(|| Some("admin".into()));
    let mut source = ConnectionConfig {
        username: username.clone(),
        password: password.clone(),
        auth_source: auth_source.clone(),
        ..ConnectionConfig::new_mongodb("mongo-sync-source", host.clone(), port)
    };
    let mut target = ConnectionConfig {
        username,
        password,
        auth_source,
        ..ConnectionConfig::new_mongodb("mongo-sync-target", host, port)
    };
    source.id = ConnectionId::new();
    target.id = ConnectionId::new();
    Some((source, target))
}

#[path = "data_sync_live/mongo_sync_tests.rs"]
mod mongo_sync_tests;

#[test]
fn memory_storage_reports_missing_connection_without_panicking() {
    let storage = MemoryStorage::new(Vec::new());
    let result = futures::executor::block_on(storage.get_connection(&ConnectionId::new()));
    assert!(matches!(result, Ok(None)));
    let error = DomainError::NotFound("missing".into());
    assert_eq!(error.message(), "missing");
}
