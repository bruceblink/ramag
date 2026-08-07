//! Redis 与 MongoDB 导入导出集成测试。

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn redis_export_import_roundtrip() {
    let Some(config) = redis_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_REDIS_* 环境变量后运行");
        return;
    };
    let svc = RedisService::new(
        Arc::new(ramag_infra_redis::RedisDriver::new()),
        Arc::new(StubStorage),
    );
    // e2e 用 DB 14（infra 集成测试占 15），前后清场
    let db: u8 = 14;
    flush_db(&svc, &config, db).await;

    let members: Vec<RedisValue> = (0..1200)
        .map(|i| RedisValue::Text(format!("m{i:04}")))
        .collect();
    svc.write_value_items(&config, db, "e2e:text", &RedisValue::Text("你好".into()))
        .await
        .unwrap();
    svc.write_value_items(
        &config,
        db,
        "e2e:bin",
        &RedisValue::Bytes(vec![0xff, 0x00, 0x01]),
    )
    .await
    .unwrap();
    svc.write_value_items(&config, db, "e2e:list", &RedisValue::List(members))
        .await
        .unwrap();
    svc.write_value_items(
        &config,
        db,
        "e2e:hash",
        &RedisValue::Hash(vec![("f".into(), RedisValue::Text("v".into()))]),
    )
    .await
    .unwrap();
    svc.write_value_items(
        &config,
        db,
        "e2e:zset",
        &RedisValue::ZSet(vec![(RedisValue::Text("m".into()), 1.5)]),
    )
    .await
    .unwrap();
    svc.write_value_items(
        &config,
        db,
        "e2e:stream",
        &RedisValue::Stream(vec![StreamEntry {
            id: "1-1".into(),
            fields: vec![("k".into(), "v".into())],
        }]),
    )
    .await
    .unwrap();
    svc.execute_command(
        &config,
        db,
        vec!["PEXPIRE".into(), "e2e:text".into(), "600000".into()],
    )
    .await
    .unwrap();

    let path = temp_file("redis.jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let summary = transfer::export_redis_db(&svc, &config, db, &path, &cancel, &progress)
        .await
        .expect("导出失败");
    assert_eq!(summary.objects, 6);
    assert!(!summary.cancelled);

    flush_db(&svc, &config, db).await;

    let summary = transfer::import_redis_db(
        &svc,
        &config,
        Some(db),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("导入失败");
    assert_eq!(summary.objects, 6);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);

    assert_eq!(svc.db_size(&config, db).await.unwrap(), 6);
    let llen = svc
        .execute_command(&config, db, vec!["LLEN".into(), "e2e:list".into()])
        .await
        .unwrap();
    assert!(matches!(llen, RedisValue::Int(1200)));
    // TTL 按导出时剩余值恢复
    let ttl = svc.key_ttl(&config, db, "e2e:text").await.unwrap();
    assert!(ttl > 0 && ttl <= 600_000, "TTL 未恢复：{ttl}");
    // 二进制保真
    let page = svc
        .read_value_page(&config, db, "e2e:bin", None, ValuePageCursor::Start, 100)
        .await
        .unwrap();
    assert!(matches!(page.items, RedisValue::Bytes(bytes) if bytes == vec![0xff, 0x00, 0x01]));

    // 可重复导入：全部 key 已存在 → 跳过
    let summary = transfer::import_redis_db(
        &svc,
        &config,
        Some(db),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.skipped, 6);
    assert_eq!(svc.db_size(&config, db).await.unwrap(), 6);

    // Redis 不支持合并策略：明确拒绝而非静默错义
    let merge_attempt = transfer::import_redis_db(
        &svc,
        &config,
        Some(db),
        &path,
        ConflictPolicy::Merge,
        &cancel,
        &progress,
    )
    .await;
    assert!(merge_attempt.is_err(), "Redis 合并导入应被拒绝");

    // 下一级导出：单 Key 与命名空间前缀都沿用整 DB 文件协议，可直接回灌。
    let key_path = temp_file("redis-key.jsonl");
    let prefix_path = temp_file("redis-prefix.jsonl");
    let summary =
        transfer::export_redis_key(&svc, &config, db, "e2e:bin", &key_path, &cancel, &progress)
            .await
            .expect("单 Key 导出失败");
    assert_eq!(summary.objects, 1);
    let summary =
        transfer::export_redis_prefix(&svc, &config, db, "e2e", &prefix_path, &cancel, &progress)
            .await
            .expect("前缀导出失败");
    assert_eq!(summary.objects, 6);

    flush_db(&svc, &config, db).await;
    let summary = transfer::import_redis_selection(
        &svc,
        &config,
        db,
        &key_path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("单 Key 导出文件回灌失败");
    assert_eq!(summary.objects, 1);
    let page = svc
        .read_value_page(&config, db, "e2e:bin", None, ValuePageCursor::Start, 100)
        .await
        .unwrap();
    assert!(matches!(page.items, RedisValue::Bytes(bytes) if bytes == vec![0xff, 0x00, 0x01]));

    flush_db(&svc, &config, db).await;
    let summary = transfer::import_redis_selection(
        &svc,
        &config,
        db,
        &prefix_path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("前缀导出文件回灌失败");
    assert_eq!(summary.objects, 6);
    assert_eq!(svc.db_size(&config, db).await.unwrap(), 6);

    flush_db(&svc, &config, db).await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&key_path);
    let _ = std::fs::remove_file(&prefix_path);
}

#[tokio::test(flavor = "multi_thread")]
async fn mongo_export_import_roundtrip() {
    let Some(config) = mongo_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MONGO_* 环境变量后运行");
        return;
    };
    let svc = MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(StubStorage),
    );
    let db = "ramag_e2e_transfer";
    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;

    // 造数：类型矩阵（Int64 / 日期 / 二进制）+ 索引 + 空集合
    let docs = vec![
        json!({"_id": 1, "big": {"$numberLong": "9007199254740993"}, "when": {"$date": "2026-01-02T03:04:05Z"}, "bin": {"$binary": {"base64": "AAEC", "subType": "00"}}}),
        json!({"_id": 2, "tags": ["a", "b"], "nested": {"x": 1.5}}),
        json!({"_id": {"$oid": "0123456789abcdef01234567"}, "note": "oid 主键"}),
    ];
    svc.insert_many(&config, db, "matrix", docs, false)
        .await
        .unwrap();
    svc.run_command(
        &config,
        db,
        json!({"createIndexes": "matrix", "indexes": [{"key": {"big": 1}, "name": "big_1", "unique": false}]}),
    )
    .await
    .unwrap();
    svc.insert_many(
        &config,
        db,
        "plain",
        vec![json!({"_id": 1}), json!({"_id": 2})],
        false,
    )
    .await
    .unwrap();
    svc.run_command(&config, db, json!({"create": "empty_coll"}))
        .await
        .unwrap();

    let path = temp_file("mongo.jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let summary = transfer::export_mongo_database(&svc, &config, db, &path, &cancel, &progress)
        .await
        .expect("导出失败");
    assert_eq!(summary.objects, 3);
    assert_eq!(summary.items, 5);

    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;

    let summary = transfer::import_mongo_database(
        &svc,
        &config,
        &path,
        None,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("导入失败");
    assert_eq!(summary.objects, 3);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);

    assert_eq!(
        svc.count(&config, db, "matrix", &Value::Null)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        svc.count(&config, db, "plain", &Value::Null).await.unwrap(),
        2
    );
    // 空集合与索引恢复
    let collections = svc.list_collections(&config, db).await.unwrap();
    assert!(collections.iter().any(|c| c.name == "empty_coll"));
    let indexes = svc
        .run_command(&config, db, json!({"listIndexes": "matrix"}))
        .await
        .unwrap();
    let index_names: Vec<&str> = indexes
        .pointer("/cursor/firstBatch")
        .and_then(Value::as_array)
        .map(|specs| {
            specs
                .iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        index_names.contains(&"big_1"),
        "索引未恢复：{index_names:?}"
    );
    // Int64 类型保真（$numberLong 不被窄化）
    let spec = MongoQuerySpec {
        filter: json!({"_id": 1}),
        ..MongoQuerySpec::default()
    };
    let found = svc.find(&config, db, "matrix", &spec).await.unwrap();
    assert_eq!(
        found
            .documents
            .first()
            .and_then(|d| d.pointer("/big/$numberLong")),
        Some(&json!("9007199254740993"))
    );

    // 可重复导入
    let summary = transfer::import_mongo_database(
        &svc,
        &config,
        &path,
        None,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.skipped, 3);

    // 合并策略：删一个文档后按 _id 补回，已存在文档不重复
    svc.delete_one(&config, db, "matrix", &json!({"_id": 2}))
        .await
        .unwrap();
    let summary = transfer::import_mongo_database(
        &svc,
        &config,
        &path,
        None,
        ConflictPolicy::Merge,
        &cancel,
        &progress,
    )
    .await
    .expect("合并导入失败");
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);
    assert_eq!(summary.skipped, 0);
    assert_eq!(
        svc.count(&config, db, "matrix", &Value::Null)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        svc.count(&config, db, "plain", &Value::Null).await.unwrap(),
        2
    );

    // 树节点单集合导出必须包含创建选项、索引与全部文档，固定集合回灌后不能退化。
    svc.run_command(
        &config,
        db,
        json!({"create": "capped_selection", "capped": true, "size": 1_048_576, "max": 100}),
    )
    .await
    .unwrap();
    svc.insert_many(
        &config,
        db,
        "capped_selection",
        vec![
            json!({"_id": 1, "sequence": 1}),
            json!({"_id": 2, "sequence": 2}),
            json!({"_id": 3, "sequence": 3}),
        ],
        false,
    )
    .await
    .unwrap();
    svc.run_command(
        &config,
        db,
        json!({"createIndexes": "capped_selection", "indexes": [{"key": {"sequence": 1}, "name": "sequence_1"}]}),
    )
    .await
    .unwrap();
    let collection_path = temp_file("mongo-collection.jsonl");
    let summary = transfer::export_mongo_collection(
        &svc,
        &config,
        (db, "capped_selection"),
        &collection_path,
        &cancel,
        &progress,
    )
    .await
    .expect("集合导出失败");
    assert_eq!((summary.objects, summary.items), (1, 3));
    let exported = std::fs::read_to_string(&collection_path).expect("读取集合导出文件");
    let marker: Value =
        serde_json::from_str(exported.lines().nth(1).expect("集合导出文件缺少结构声明"))
            .expect("集合结构声明不是合法 JSON");
    assert_eq!(
        marker.pointer("/options/capped").and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        marker
            .get("indexes")
            .and_then(Value::as_array)
            .is_some_and(|items| items
                .iter()
                .any(|item| { item.get("name").and_then(Value::as_str) == Some("sequence_1") }))
    );
    assert!(!exported.contains("\"collection\":\"matrix\""));

    svc.run_command(&config, db, json!({"drop": "capped_selection"}))
        .await
        .unwrap();
    let summary = transfer::import_mongo_collection(
        &svc,
        &config,
        &collection_path,
        db,
        ConflictPolicy::Fail,
        &cancel,
        &progress,
    )
    .await
    .expect("集合导出文件回灌失败");
    assert_eq!((summary.objects, summary.items), (1, 3));
    assert_eq!(
        svc.count(&config, db, "capped_selection", &Value::Null)
            .await
            .unwrap(),
        3
    );
    let options = svc
        .run_command(
            &config,
            db,
            json!({"listCollections": 1, "filter": {"name": "capped_selection"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        options
            .pointer("/cursor/firstBatch/0/options/capped")
            .and_then(Value::as_bool),
        Some(true),
        "固定集合创建选项未恢复：{options}"
    );
    let indexes = svc
        .run_command(&config, db, json!({"listIndexes": "capped_selection"}))
        .await
        .unwrap();
    assert!(
        indexes
            .pointer("/cursor/firstBatch")
            .and_then(Value::as_array)
            .is_some_and(|items| items
                .iter()
                .any(|item| { item.get("name").and_then(Value::as_str) == Some("sequence_1") })),
        "集合索引未恢复：{indexes}"
    );

    // time-series 也是集合结构：创建选项必须先恢复，不能隐式建成普通集合。
    svc.run_command(
        &config,
        db,
        json!({"create": "metrics_selection", "timeseries": {"timeField": "observedAt", "metaField": "metadata", "granularity": "minutes"}}),
    )
    .await
    .unwrap();
    svc.insert_many(
        &config,
        db,
        "metrics_selection",
        vec![
            json!({"observedAt": {"$date": "2026-01-01T00:00:00Z"}, "metadata": {"sensor": "a"}, "value": 1}),
            json!({"observedAt": {"$date": "2026-01-01T00:01:00Z"}, "metadata": {"sensor": "a"}, "value": 2}),
        ],
        false,
    )
    .await
    .unwrap();
    let timeseries_path = temp_file("mongo-timeseries.jsonl");
    transfer::export_mongo_collection(
        &svc,
        &config,
        (db, "metrics_selection"),
        &timeseries_path,
        &cancel,
        &progress,
    )
    .await
    .expect("time-series 导出失败");
    svc.run_command(&config, db, json!({"drop": "metrics_selection"}))
        .await
        .unwrap();
    transfer::import_mongo_collection(
        &svc,
        &config,
        &timeseries_path,
        db,
        ConflictPolicy::Fail,
        &cancel,
        &progress,
    )
    .await
    .expect("time-series 回灌失败");
    let options = svc
        .run_command(
            &config,
            db,
            json!({"listCollections": 1, "filter": {"name": "metrics_selection"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        options
            .pointer("/cursor/firstBatch/0/options/timeseries/timeField")
            .and_then(Value::as_str),
        Some("observedAt"),
        "time-series 创建选项未恢复：{options}"
    );
    assert_eq!(
        svc.count(&config, db, "metrics_selection", &Value::Null)
            .await
            .unwrap(),
        2
    );

    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&collection_path);
    let _ = std::fs::remove_file(&timeseries_path);
}
