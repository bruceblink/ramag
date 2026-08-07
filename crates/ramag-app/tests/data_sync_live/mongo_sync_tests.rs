//! MongoDB 数据同步集成测试。

use super::*;

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
    let gate = Arc::new(DataSyncGate::default());
    let sync_service =
        DataSyncService::new(connection_service, mongo_service.clone(), gate.clone());

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
