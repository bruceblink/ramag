#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! 数据同步真实数据库测试。缺对应 RAMAG_TEST_* 环境变量时软跳过。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ramag_app::{
    ConnectionService, DataSyncConfirmation, DataSyncGate, DataSyncGatePhase, DataSyncService,
    MongoService, RedisService,
};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DataSyncRequest, DataSyncScope, DataSyncTaskId, DriverKind,
    MongoQuerySpec, MongoSyncScope, QueryRecord, QueryRecordId, RedisSyncScope, RedisValue,
    StreamEntry, SyncObjectMapping, SyncObjectSelection,
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

fn redis_configs() -> Option<(ConnectionConfig, ConnectionConfig)> {
    let host = std::env::var("RAMAG_TEST_REDIS_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_REDIS_PORT").ok()?.parse().ok()?;
    let password = std::env::var("RAMAG_TEST_REDIS_PASSWORD").unwrap_or_default();
    let username = std::env::var("RAMAG_TEST_REDIS_USERNAME").unwrap_or_default();
    let mut source = ConnectionConfig {
        username: username.clone(),
        password: password.clone(),
        ..ConnectionConfig::new_redis("sync-source", host.clone(), port)
    };
    let mut target = ConnectionConfig {
        username,
        password,
        ..ConnectionConfig::new_redis("sync-target", host, port)
    };
    source.id = ConnectionId::new();
    target.id = ConnectionId::new();
    Some((source, target))
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

fn request(
    source: &ConnectionConfig,
    target: &ConnectionConfig,
    source_db: u8,
    target_db: u8,
) -> DataSyncRequest {
    DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Redis,
        scope: DataSyncScope::Redis(RedisSyncScope::Database {
            source_db,
            target_db,
            target_prefix: "copy:".into(),
        }),
    }
}

async fn flush(service: &RedisService, config: &ConnectionConfig, db: u8) {
    service
        .execute_command(config, db, vec!["FLUSHDB".into()])
        .await
        .expect("清理 Redis 测试 DB");
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_sync_is_non_overwriting_repeatable_guarded_and_cancellable() {
    let Some((source, target)) = redis_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_REDIS_HOST/PORT 后运行数据同步测试");
        return;
    };
    const SOURCE_DB: u8 = 12;
    const TARGET_DB: u8 = 13;

    let storage: Arc<dyn Storage> =
        Arc::new(MemoryStorage::new(vec![source.clone(), target.clone()]));
    let connection_service = Arc::new(ConnectionService::new(
        HashMap::<DriverKind, Arc<dyn Driver>>::new(),
        storage.clone(),
    ));
    let redis_service = Arc::new(RedisService::new(
        Arc::new(ramag_infra_redis::RedisDriver::new()),
        storage,
    ));
    let gate = Arc::new(DataSyncGate::default());
    let mongo_service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(MemoryStorage::new(Vec::new())),
    ));
    let sync_service = DataSyncService::new(
        connection_service,
        redis_service.clone(),
        mongo_service,
        gate.clone(),
    );

    flush(&redis_service, &source, SOURCE_DB).await;
    flush(&redis_service, &target, TARGET_DB).await;
    redis_service
        .write_value_items(
            &source,
            SOURCE_DB,
            "keep",
            &RedisValue::Text("source-must-not-overwrite".into()),
        )
        .await
        .expect("写源已有冲突 Key");
    redis_service
        .write_value_items(
            &source,
            SOURCE_DB,
            "new",
            &RedisValue::Text("new-value".into()),
        )
        .await
        .expect("写源缺失 Key");
    redis_service
        .write_value_items(
            &source,
            SOURCE_DB,
            "list",
            &RedisValue::List(vec![
                RedisValue::Text("a".into()),
                RedisValue::Text("a".into()),
                RedisValue::Text("b".into()),
            ]),
        )
        .await
        .expect("写源 List");
    for (key, value) in [
        ("empty", RedisValue::Text(String::new())),
        ("binary", RedisValue::Bytes(vec![0, 159, 255, 10])),
        (
            "hash",
            RedisValue::Hash(vec![
                ("name".into(), RedisValue::Text("ramag".into())),
                ("blob".into(), RedisValue::Bytes(vec![0, 255])),
            ]),
        ),
        (
            "set",
            RedisValue::Set(vec![
                RedisValue::Text("a".into()),
                RedisValue::Text("b".into()),
            ]),
        ),
        (
            "zset",
            RedisValue::ZSet(vec![
                (RedisValue::Text("low".into()), -1.25),
                (RedisValue::Bytes(vec![0, 255]), 3.5),
            ]),
        ),
        (
            "stream",
            RedisValue::Stream(vec![
                StreamEntry {
                    id: "1-0".into(),
                    fields: vec![("event".into(), "created".into())],
                },
                StreamEntry {
                    id: "2-0".into(),
                    fields: vec![("event".into(), "updated".into())],
                },
            ]),
        ),
        (
            "large-list",
            RedisValue::List(
                (0..5_001)
                    .map(|index| RedisValue::Text(format!("item-{index}")))
                    .collect(),
            ),
        ),
    ] {
        redis_service
            .write_value_items(&source, SOURCE_DB, key, &value)
            .await
            .unwrap_or_else(|error| panic!("写源 {key} 失败：{error}"));
    }
    redis_service
        .set_ttl_ms(&source, SOURCE_DB, "new", 60_000)
        .await
        .expect("设置源 TTL");
    redis_service
        .write_value_items(
            &target,
            TARGET_DB,
            "copy:keep",
            &RedisValue::Text("target-kept".into()),
        )
        .await
        .expect("写目标已有 Key");
    redis_service
        .set_ttl_ms(&target, TARGET_DB, "copy:keep", 120_000)
        .await
        .expect("设置目标已有 TTL");
    redis_service
        .write_value_items(
            &target,
            TARGET_DB,
            "unrelated",
            &RedisValue::Text("untouched".into()),
        )
        .await
        .expect("写目标无关 Key");

    let redis_scopes = sync_service
        .list_catalog_scopes(&source)
        .await
        .expect("读取 Redis DB 目录");
    assert_eq!(redis_scopes.len(), 256);
    assert_eq!(redis_scopes.first().map(String::as_str), Some("0"));
    assert_eq!(redis_scopes.last().map(String::as_str), Some("255"));
    let source_catalog = sync_service
        .list_catalog_objects(&source, &SOURCE_DB.to_string())
        .await
        .expect("读取 Redis Key 目录");
    assert!(!source_catalog.truncated);
    assert!(source_catalog.names.contains(&"new".to_string()));
    assert!(source_catalog.names.contains(&"large-list".to_string()));

    let prepared = sync_service
        .preflight(request(&source, &target, SOURCE_DB, TARGET_DB))
        .await
        .expect("Redis 同步预检");
    assert!(prepared.report().requires_second_confirmation);
    let wrong_confirmation =
        sync_service.start(prepared, DataSyncConfirmation::CreateMissingTargets);
    assert!(wrong_confirmation.is_err(), "已有目标必须二次确认");
    assert!(!gate.is_blocking());

    let prepared = sync_service
        .preflight(request(&source, &target, SOURCE_DB, TARGET_DB))
        .await
        .expect("重新预检");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("最终确认后应开始占屏");
    let permit = started.permit().clone();
    assert!(gate.is_blocking());
    sync_service.execute(started).await;

    let snapshot = gate.snapshot().expect("完成结果应继续占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Completed);
    let summary = snapshot.summary.expect("完成汇总");
    assert_eq!(summary.inserted, 9);
    assert_eq!(summary.skipped, 1);
    assert_eq!(redis_service.db_size(&source, SOURCE_DB).await.unwrap(), 10);
    assert_eq!(redis_service.db_size(&target, TARGET_DB).await.unwrap(), 11);
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "copy:keep")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "target-kept"
    ));
    let kept_ttl = redis_service
        .key_ttl(&target, TARGET_DB, "copy:keep")
        .await
        .unwrap();
    assert!((1..=120_000).contains(&kept_ttl));
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "copy:new")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "new-value"
    ));
    let copied_ttl = redis_service
        .key_ttl(&target, TARGET_DB, "copy:new")
        .await
        .unwrap();
    assert!((1..=60_000).contains(&copied_ttl));
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "unrelated")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "untouched"
    ));
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "copy:empty")
            .await
            .unwrap(),
        RedisValue::Text(value) if value.is_empty()
    ));
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "copy:binary")
            .await
            .unwrap(),
        RedisValue::Bytes(value) if value == vec![0, 159, 255, 10]
    ));
    for (command, expected) in [
        ("HLEN", 2),
        ("SCARD", 2),
        ("ZCARD", 2),
        ("XLEN", 2),
        ("LLEN", 5_001),
    ] {
        let key = match command {
            "HLEN" => "copy:hash",
            "SCARD" => "copy:set",
            "ZCARD" => "copy:zset",
            "XLEN" => "copy:stream",
            "LLEN" => "copy:large-list",
            _ => unreachable!(),
        };
        assert!(matches!(
            redis_service
                .execute_command(&target, TARGET_DB, vec![command.into(), key.into()])
                .await
                .unwrap(),
            RedisValue::Int(value) if value == expected
        ));
    }
    let temporary = redis_service
        .scan_batch(
            &target,
            TARGET_DB,
            0,
            Some("__ramag_sync_tmp__:*"),
            None,
            100,
        )
        .await
        .expect("扫描同步临时 Key");
    assert!(temporary.keys.is_empty(), "成功后不得残留临时 Key");
    assert!(sync_service.acknowledge_result(&permit));
    assert!(!gate.is_blocking());

    // 连续第二次必须零新增，目标已有内容保持原样。
    let prepared = sync_service
        .preflight(request(&source, &target, SOURCE_DB, TARGET_DB))
        .await
        .expect("重复同步预检");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("重复同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let summary = gate
        .snapshot()
        .and_then(|snapshot| snapshot.summary)
        .expect("重复同步汇总");
    assert_eq!(summary.inserted, 0);
    assert_eq!(summary.skipped, 10);
    assert_eq!(redis_service.db_size(&target, TARGET_DB).await.unwrap(), 11);
    assert!(sync_service.acknowledge_result(&permit));

    // 预检后目标变化必须中止，不能用过期确认继续写。
    let prepared = sync_service
        .preflight(request(&source, &target, SOURCE_DB, TARGET_DB))
        .await
        .expect("目标变化测试预检");
    redis_service
        .write_value_items(
            &target,
            TARGET_DB,
            "changed-after-preflight",
            &RedisValue::Text("changed".into()),
        )
        .await
        .expect("制造目标变化");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("确认阶段仍可开始，执行前复核应失败");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let snapshot = gate.snapshot().expect("失败结果应占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Failed);
    assert!(
        snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.contains("预检后变化"))
    );
    assert!(sync_service.acknowledge_result(&permit));

    // 取消不是关闭：安全停止后进入 Cancelled，仍需用户确认才释放门禁。
    let prepared = sync_service
        .preflight(request(&source, &target, SOURCE_DB, TARGET_DB))
        .await
        .expect("取消测试预检");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("取消测试开始");
    let permit = started.permit().clone();
    assert!(sync_service.request_cancel(&permit));
    sync_service.execute(started).await;
    assert_eq!(
        gate.snapshot().map(|snapshot| snapshot.phase),
        Some(DataSyncGatePhase::Cancelled)
    );
    assert!(gate.is_blocking());
    assert!(sync_service.acknowledge_result(&permit));

    // 前缀范围必须把 glob 元字符当作普通文本，并保持后缀映射。
    flush(&redis_service, &source, SOURCE_DB).await;
    flush(&redis_service, &target, TARGET_DB).await;
    for (key, value) in [
        ("old[*]:1", "source-one"),
        ("old[*]:2", "source-two"),
        ("oldx:3", "outside-range"),
    ] {
        redis_service
            .write_value_items(&source, SOURCE_DB, key, &RedisValue::Text(value.into()))
            .await
            .expect("写前缀范围源 Key");
    }
    redis_service
        .write_value_items(
            &target,
            TARGET_DB,
            "new?:1",
            &RedisValue::Text("target-kept".into()),
        )
        .await
        .expect("写前缀范围目标已有 Key");
    let prefix_request = DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Redis,
        scope: DataSyncScope::Redis(RedisSyncScope::Prefix {
            source_db: SOURCE_DB,
            target_db: TARGET_DB,
            source_prefix: "old[*]:".into(),
            target_prefix: "new?:".into(),
        }),
    };
    let prepared = sync_service
        .preflight(prefix_request)
        .await
        .expect("Redis 前缀同步预检");
    assert!(prepared.report().requires_second_confirmation);
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("Redis 前缀同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let summary = gate
        .snapshot()
        .and_then(|snapshot| snapshot.summary)
        .expect("Redis 前缀同步汇总");
    assert_eq!(summary.inserted, 1);
    assert_eq!(summary.skipped, 1);
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "new?:1")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "target-kept"
    ));
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "new?:2")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "source-two"
    ));
    assert!(
        redis_service
            .get_value(&target, TARGET_DB, "new?:3")
            .await
            .unwrap()
            .is_nil(),
        "前缀范围外 Key 不能被同步"
    );
    assert!(sync_service.acknowledge_result(&permit));

    // 指定 Key 支持改名；源 Key 不存在时只警告并安全跳过。
    let keys_request = DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Redis,
        scope: DataSyncScope::Redis(RedisSyncScope::Keys {
            source_db: SOURCE_DB,
            target_db: TARGET_DB,
            mappings: vec![
                ramag_domain::entities::RedisKeyMapping {
                    source: "old[*]:1".into(),
                    target: "renamed-one".into(),
                },
                ramag_domain::entities::RedisKeyMapping {
                    source: "missing-source".into(),
                    target: "renamed-missing".into(),
                },
            ],
        }),
    };
    let prepared = sync_service
        .preflight(keys_request)
        .await
        .expect("Redis 指定 Key 同步预检");
    assert!(!prepared.report().requires_second_confirmation);
    assert!(
        prepared
            .report()
            .warnings
            .iter()
            .any(|warning| warning.contains("missing-source"))
    );
    let started = sync_service
        .start(prepared, DataSyncConfirmation::CreateMissingTargets)
        .expect("Redis 指定 Key 同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let summary = gate
        .snapshot()
        .and_then(|snapshot| snapshot.summary)
        .expect("Redis 指定 Key 同步汇总");
    assert_eq!(summary.inserted, 1);
    assert_eq!(summary.skipped, 1);
    assert!(matches!(
        redis_service
            .get_value(&target, TARGET_DB, "renamed-one")
            .await
            .unwrap(),
        RedisValue::Text(value) if value == "source-one"
    ));
    assert!(sync_service.acknowledge_result(&permit));

    flush(&redis_service, &source, SOURCE_DB).await;
    flush(&redis_service, &target, TARGET_DB).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mongo_sync_creates_and_fills_without_overwriting_then_repeats_safely() {
    let Some((source, target)) = mongo_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MONGO_HOST/PORT 后运行数据同步测试");
        return;
    };
    let suffix = std::process::id();
    let source_db = format!("ramag_sync_source_{suffix}");
    let target_db = format!("ramag_sync_target_{suffix}");
    let new_target_db = format!("ramag_sync_new_target_{suffix}");
    let storage: Arc<dyn Storage> =
        Arc::new(MemoryStorage::new(vec![source.clone(), target.clone()]));
    let connection_service = Arc::new(ConnectionService::new(
        HashMap::<DriverKind, Arc<dyn Driver>>::new(),
        storage.clone(),
    ));
    let mongo_service = Arc::new(MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        storage.clone(),
    ));
    let redis_service = Arc::new(RedisService::new(
        Arc::new(ramag_infra_redis::RedisDriver::new()),
        storage,
    ));
    let gate = Arc::new(DataSyncGate::default());
    let sync_service = DataSyncService::new(
        connection_service,
        redis_service,
        mongo_service.clone(),
        gate.clone(),
    );

    let _ = mongo_service
        .run_command(&source, &source_db, json!({"dropDatabase": 1}))
        .await;
    let _ = mongo_service
        .run_command(&target, &target_db, json!({"dropDatabase": 1}))
        .await;
    let _ = mongo_service
        .run_command(&target, &new_target_db, json!({"dropDatabase": 1}))
        .await;
    let validator = json!({"$jsonSchema": {"bsonType": "object", "required": ["email"]}});
    mongo_service
        .run_command(
            &source,
            &source_db,
            json!({"create": "users", "validator": validator}),
        )
        .await
        .expect("创建源 users");
    mongo_service
        .run_command(&source, &source_db, json!({"create": "logs"}))
        .await
        .expect("创建源 logs");
    mongo_service
        .run_command(&source, &source_db, json!({"create": "mixed"}))
        .await
        .expect("创建源 mixed");
    mongo_service
        .run_command(&source, &source_db, json!({"create": "bulk"}))
        .await
        .expect("创建源 bulk");
    mongo_service
        .run_command(
            &source,
            &source_db,
            json!({
                "createIndexes": "users",
                "indexes": [{"key": {"email": 1}, "name": "email_1", "unique": true}]
            }),
        )
        .await
        .expect("创建源索引");
    mongo_service
        .insert_many(
            &source,
            &source_db,
            "users",
            vec![
                json!({"_id": 1, "email": "one@example.com", "name": "source-one"}),
                json!({"_id": 2, "email": "two@example.com", "name": "source-two"}),
                json!({"_id": 3, "email": "three@example.com", "name": "source-three"}),
            ],
            false,
        )
        .await
        .expect("写入源 users");
    mongo_service
        .insert_many(
            &source,
            &source_db,
            "logs",
            vec![
                json!({"_id": "a", "message": "A"}),
                json!({"_id": "b", "message": "B"}),
            ],
            false,
        )
        .await
        .expect("写入源 logs");
    mongo_service
        .insert_many(
            &source,
            &source_db,
            "mixed",
            vec![
                json!({"_id": 7, "kind": "number"}),
                json!({"_id": "seven", "kind": "string"}),
                json!({"_id": {"$oid": "64b000000000000000000001"}, "kind": "object-id"}),
            ],
            false,
        )
        .await
        .expect("写入混合类型 _id");
    let bulk_documents: Vec<Value> = (1..=5_001)
        .map(|id| json!({"_id": id, "value": format!("source-{id}")}))
        .collect();
    mongo_service
        .insert_many(
            &source,
            &source_db,
            "bulk",
            bulk_documents[..5_000].to_vec(),
            false,
        )
        .await
        .expect("写入源 bulk 第一批");
    mongo_service
        .insert_many(
            &source,
            &source_db,
            "bulk",
            bulk_documents[5_000..].to_vec(),
            false,
        )
        .await
        .expect("写入源 bulk 边界记录");

    let mongo_scopes = sync_service
        .list_catalog_scopes(&source)
        .await
        .expect("读取 MongoDB Database 目录");
    assert!(mongo_scopes.contains(&source_db));
    let mongo_catalog = sync_service
        .list_catalog_objects(&source, &source_db)
        .await
        .expect("读取 MongoDB Collection 目录");
    assert!(!mongo_catalog.truncated);
    for collection in ["users", "logs", "mixed", "bulk"] {
        assert!(mongo_catalog.names.contains(&collection.to_string()));
    }

    // 目标 Database 和 users Collection 已存在；同 `_id` 内容故意不同，另有额外文档。
    mongo_service
        .run_command(
            &target,
            &target_db,
            json!({"create": "archive_users", "validator": validator}),
        )
        .await
        .expect("创建目标已有 Collection");
    mongo_service
        .insert_many(
            &target,
            &target_db,
            "archive_users",
            vec![
                json!({"_id": 1, "email": "target-one@example.com", "name": "target-kept"}),
                json!({"_id": 99, "email": "extra@example.com", "name": "extra-kept"}),
            ],
            false,
        )
        .await
        .expect("写入目标已有文档");
    mongo_service
        .run_command(&target, &target_db, json!({"create": "archive_bulk"}))
        .await
        .expect("创建目标 bulk");
    mongo_service
        .insert_many(
            &target,
            &target_db,
            "archive_bulk",
            vec![
                json!({"_id": 1, "value": "target-first"}),
                json!({"_id": 5_001, "value": "target-last"}),
            ],
            false,
        )
        .await
        .expect("写入目标 bulk 边界记录");

    let make_request = || DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Mongodb,
        scope: DataSyncScope::Mongo(MongoSyncScope {
            source_database: source_db.clone(),
            target_database: target_db.clone(),
            collections: SyncObjectSelection::Selected(vec![
                SyncObjectMapping {
                    source: "users".into(),
                    target: "archive_users".into(),
                },
                SyncObjectMapping {
                    source: "logs".into(),
                    target: "archive_logs".into(),
                },
                SyncObjectMapping {
                    source: "mixed".into(),
                    target: "archive_mixed".into(),
                },
                SyncObjectMapping {
                    source: "bulk".into(),
                    target: "archive_bulk".into(),
                },
            ]),
        }),
    };

    let prepared = sync_service
        .preflight(make_request())
        .await
        .expect("MongoDB 同步预检");
    assert!(prepared.report().requires_second_confirmation);
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("MongoDB 同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let snapshot = gate.snapshot().expect("MongoDB 完成结果应占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Completed);
    let summary = snapshot.summary.expect("MongoDB 完成汇总");
    assert_eq!(summary.inserted, 5_006);
    assert_eq!(summary.skipped, 3);

    let user_one = mongo_service
        .find(
            &target,
            &target_db,
            "archive_users",
            &MongoQuerySpec {
                filter: json!({"_id": 1}),
                limit: Some(1),
                ..MongoQuerySpec::default()
            },
        )
        .await
        .expect("读取目标已有文档")
        .documents
        .into_iter()
        .next()
        .expect("目标 id=1 应存在");
    assert_eq!(
        user_one.get("name"),
        Some(&Value::String("target-kept".into()))
    );
    assert_eq!(
        mongo_service
            .count(&target, &target_db, "archive_users", &json!({}))
            .await
            .unwrap(),
        4,
        "补入 2、3，保留原 1、99"
    );
    assert_eq!(
        mongo_service
            .count(&target, &target_db, "archive_logs", &json!({}))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        mongo_service
            .count(&target, &target_db, "archive_mixed", &json!({}))
            .await
            .unwrap(),
        3,
        "不同 BSON 类型的 _id 均应完成 keyset 同步"
    );
    assert_eq!(
        mongo_service
            .count(&target, &target_db, "archive_bulk", &json!({}))
            .await
            .unwrap(),
        5_001,
        "5,001 条记录必须跨分页边界完整补齐"
    );
    let first_bulk = mongo_service
        .find(
            &target,
            &target_db,
            "archive_bulk",
            &MongoQuerySpec {
                filter: json!({"_id": 1}),
                limit: Some(1),
                ..MongoQuerySpec::default()
            },
        )
        .await
        .expect("读取目标 bulk 已有记录");
    assert_eq!(
        first_bulk
            .documents
            .first()
            .and_then(|document| document.get("value")),
        Some(&Value::String("target-first".into())),
        "分页同步不能覆盖目标已有内容"
    );
    let indexes = mongo_service
        .run_command(&target, &target_db, json!({"listIndexes": "archive_users"}))
        .await
        .expect("读取目标索引");
    assert!(indexes.to_string().contains("email_1"));
    assert!(sync_service.acknowledge_result(&permit));

    // 第二次不新增、不更新、不删除。
    let prepared = sync_service
        .preflight(make_request())
        .await
        .expect("MongoDB 重复同步预检");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("MongoDB 重复同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    let summary = gate
        .snapshot()
        .and_then(|snapshot| snapshot.summary)
        .expect("MongoDB 重复同步汇总");
    assert_eq!(summary.inserted, 0);
    assert_eq!(summary.skipped, 5_009);
    assert!(sync_service.acknowledge_result(&permit));

    // 目标 Database 完全不存在时，应按新名称创建并迁移所选 Collection。
    let new_database_request = DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Mongodb,
        scope: DataSyncScope::Mongo(MongoSyncScope {
            source_database: source_db.clone(),
            target_database: new_target_db.clone(),
            collections: SyncObjectSelection::Selected(vec![SyncObjectMapping {
                source: "logs".into(),
                target: "renamed_logs".into(),
            }]),
        }),
    };
    let prepared = sync_service
        .preflight(new_database_request)
        .await
        .expect("MongoDB 新目标库预检");
    assert!(!prepared.report().requires_second_confirmation);
    let started = sync_service
        .start(prepared, DataSyncConfirmation::CreateMissingTargets)
        .expect("MongoDB 新目标库同步开始");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    assert_eq!(
        gate.snapshot()
            .and_then(|snapshot| snapshot.summary)
            .map(|summary| summary.inserted),
        Some(2)
    );
    assert_eq!(
        mongo_service
            .count(&target, &new_target_db, "renamed_logs", &json!({}))
            .await
            .unwrap(),
        2
    );
    assert!(sync_service.acknowledge_result(&permit));

    // 视图和同名但定义不同的索引必须在预检阶段明确拒绝。
    mongo_service
        .run_command(
            &source,
            &source_db,
            json!({"create": "users_view", "viewOn": "users", "pipeline": []}),
        )
        .await
        .expect("创建源视图");
    let view_request = DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Mongodb,
        scope: DataSyncScope::Mongo(MongoSyncScope {
            source_database: source_db.clone(),
            target_database: target_db.clone(),
            collections: SyncObjectSelection::Selected(vec![SyncObjectMapping {
                source: "users_view".into(),
                target: "users_view_copy".into(),
            }]),
        }),
    };
    assert!(
        sync_service
            .preflight(view_request)
            .await
            .err()
            .expect("MongoDB 视图必须拒绝")
            .message()
            .contains("视图")
    );

    for (config, database, collection, direction) in [
        (&source, &source_db, "index_source", 1),
        (&target, &target_db, "index_target", -1),
    ] {
        mongo_service
            .run_command(config, database, json!({"create": collection}))
            .await
            .expect("创建索引兼容性 Collection");
        mongo_service
            .run_command(
                config,
                database,
                json!({
                    "createIndexes": collection,
                    "indexes": [{"key": {"code": direction}, "name": "code_1"}]
                }),
            )
            .await
            .expect("创建索引兼容性索引");
    }
    let incompatible_request = DataSyncRequest {
        task_id: DataSyncTaskId::new(),
        source_connection_id: source.id.clone(),
        target_connection_id: target.id.clone(),
        engine: DriverKind::Mongodb,
        scope: DataSyncScope::Mongo(MongoSyncScope {
            source_database: source_db.clone(),
            target_database: target_db.clone(),
            collections: SyncObjectSelection::Selected(vec![SyncObjectMapping {
                source: "index_source".into(),
                target: "index_target".into(),
            }]),
        }),
    };
    assert!(
        sync_service
            .preflight(incompatible_request)
            .await
            .err()
            .expect("不同定义的同名索引必须拒绝")
            .message()
            .contains("索引 code_1")
    );

    // 预检后新增索引会改变目标结构指纹，执行前必须失败。
    let prepared = sync_service
        .preflight(make_request())
        .await
        .expect("MongoDB 结构变化测试预检");
    mongo_service
        .run_command(
            &target,
            &target_db,
            json!({
                "createIndexes": "archive_logs",
                "indexes": [{"key": {"message": 1}, "name": "message_1"}]
            }),
        )
        .await
        .expect("制造目标索引变化");
    let started = sync_service
        .start(prepared, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("结构复核前进入占屏");
    let permit = started.permit().clone();
    sync_service.execute(started).await;
    assert_eq!(
        gate.snapshot().map(|snapshot| snapshot.phase),
        Some(DataSyncGatePhase::Failed)
    );
    assert!(sync_service.acknowledge_result(&permit));

    mongo_service
        .run_command(&source, &source_db, json!({"dropDatabase": 1}))
        .await
        .expect("清理源测试库");
    mongo_service
        .run_command(&target, &target_db, json!({"dropDatabase": 1}))
        .await
        .expect("清理目标测试库");
    mongo_service
        .run_command(&target, &new_target_db, json!({"dropDatabase": 1}))
        .await
        .expect("清理新目标测试库");
}

#[test]
fn memory_storage_reports_missing_connection_without_panicking() {
    let storage = MemoryStorage::new(Vec::new());
    let result = futures::executor::block_on(storage.get_connection(&ConnectionId::new()));
    assert!(matches!(result, Ok(None)));
    let error = DomainError::NotFound("missing".into());
    assert_eq!(error.message(), "missing");
}
