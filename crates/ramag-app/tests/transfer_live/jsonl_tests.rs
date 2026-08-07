//! JSONL 导入集成测试。

use super::*;

// 覆盖缺列、未知键、脏行以及三种冲突策略。
#[tokio::test(flavor = "multi_thread")]
async fn mysql_jsonl_table_import() {
    let Some(config) = mysql_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MYSQL_* 环境变量后运行");
        return;
    };
    let svc = sql_service();
    let db = "ramag_e2e_jsonl";
    exec(&svc, &config, format!("DROP DATABASE IF EXISTS `{db}`;")).await;
    exec(
        &svc,
        &config,
        format!(
            "CREATE DATABASE `{db}`;\n\
             CREATE TABLE `{db}`.`items` (\
             `id` INT NOT NULL AUTO_INCREMENT PRIMARY KEY, \
             `name` VARCHAR(64) NOT NULL, \
             `qty` INT NULL DEFAULT 7, \
             `note` TEXT NULL, \
             `payload` VARBINARY(8) NULL, \
             KEY `items_name_idx` (`name`)); \
             CREATE TABLE `{db}`.`unrelated` (`id` INT PRIMARY KEY);"
        ),
    )
    .await;

    let path = temp_file("mysql-table.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"id\":1,\"name\":\"甲\",\"qty\":1,\"note\":\"a'b\\\\c\"}\n",
            "{\"id\":2,\"name\":\"乙\"}\n",
            "{\"name\":\"丙\",\"ghost\":true}\n",
            "not json\n",
            "{\"just\":\"unknown\"}\n",
        ),
    )
    .expect("写测试 jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let count_sql = format!("SELECT COUNT(*) FROM `{db}`.`items`;");

    // 首次导入：3 行入库、2 行脏数据计失败、未知键 ghost 有告警
    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("首次导入失败");
    assert_eq!(summary.items, 3, "警告：{:?}", summary.warnings);
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.skipped, 0);
    assert!(summary.warnings.iter().any(|w| w.contains("ghost")));
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);
    // 缺列走库默认值；含引号 / 反斜杠文本保真
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT `qty` FROM `{db}`.`items` WHERE `id` = 2;")
        )
        .await,
        7
    );
    match scalar_value(
        &svc,
        &config,
        &format!("SELECT `note` FROM `{db}`.`items` WHERE `id` = 1;"),
    )
    .await
    {
        ramag_domain::entities::Value::Text(text) => assert_eq!(text, "a'b\\c"),
        other => panic!("期望文本 note，实得 {other:?}"),
    }

    // Skip 重复导入：显式 id 冲突跳过，无主键行（自增）再插一条
    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.items, 1);
    assert_eq!(summary.skipped, 2);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 4);

    // Overwrite：先清空再导入
    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Overwrite,
        &cancel,
        &progress,
    )
    .await
    .expect("覆盖导入失败");
    assert_eq!(summary.items, 3);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);

    // Fail：遇到第一个冲突行即报错，表保持不变
    let failed = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Fail,
        &cancel,
        &progress,
    )
    .await;
    assert!(failed.is_err(), "Fail 策略应在冲突时报错");
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);

    // 树节点导出是结构化 SQL：只含目标表的 DDL 与全量数据，可在删表后完整恢复。
    exec(
        &svc,
        &config,
        format!("UPDATE `{db}`.`items` SET `payload` = X'FF00' WHERE `id` = 1;"),
    )
    .await;
    let export_path = temp_file("mysql-table-export.sql");
    let summary = transfer::export_sql_table(
        &svc,
        &config,
        (db, "items"),
        &export_path,
        &cancel,
        &progress,
    )
    .await
    .expect("表导出失败");
    assert_eq!((summary.objects, summary.items), (1, 3));
    let exported = std::fs::read_to_string(&export_path).expect("读取单表 SQL 导出文件");
    assert!(exported.contains("CREATE TABLE `items`"));
    assert!(exported.contains("INSERT INTO `items`"));
    assert!(exported.contains("items_name_idx"));
    assert!(!exported.contains("`unrelated`"));
    let summary = transfer::import_sql_table(
        &svc,
        &config,
        &export_path,
        db,
        ConflictPolicy::Overwrite,
        &cancel,
        &progress,
    )
    .await
    .expect("表导出文件回灌失败");
    assert_eq!(summary.objects, 1);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!(
                "SELECT COUNT(*) FROM information_schema.STATISTICS \
                 WHERE TABLE_SCHEMA = '{db}' AND TABLE_NAME = 'items' \
                   AND INDEX_NAME = 'items_name_idx';"
            ),
        )
        .await,
        1
    );
    let payload = scalar_value(
        &svc,
        &config,
        &format!("SELECT `payload` FROM `{db}`.`items` WHERE `id` = 1;"),
    )
    .await;
    assert!(
        matches!(payload, ramag_domain::entities::Value::Bytes(bytes) if bytes == vec![0xff, 0x00])
    );

    exec(&svc, &config, format!("DROP DATABASE `{db}`;")).await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&export_path);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_jsonl_table_import() {
    let Some(config) = pg_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 环境变量后运行");
        return;
    };
    let svc = sql_service();
    let schema = "ramag_e2e_jsonl_pg";
    exec(
        &svc,
        &config,
        format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE;"),
    )
    .await;
    exec(
        &svc,
        &config,
        format!(
            "CREATE SCHEMA \"{schema}\";\n\
             CREATE TYPE \"{schema}\".\"item_state\" AS ENUM ('ready', 'done'); \
             CREATE TYPE \"{schema}\".\"unused_state\" AS ENUM ('unused'); \
             CREATE TABLE \"{schema}\".\"unrelated\" (\"id\" INT PRIMARY KEY); \
             CREATE TABLE \"{schema}\".\"items\" (\
             \"id\" SERIAL PRIMARY KEY, \
             \"name\" TEXT NOT NULL, \
             \"qty\" INT DEFAULT 7, \
             \"note\" TEXT, \
             \"payload\" BYTEA, \
             \"state\" \"{schema}\".\"item_state\" NOT NULL DEFAULT 'ready', \
             \"parent_id\" INT REFERENCES \"{schema}\".\"unrelated\"(\"id\")); \
             CREATE INDEX \"items_name_idx\" ON \"{schema}\".\"items\" (\"name\"); \
             COMMENT ON TABLE \"{schema}\".\"items\" IS 'single export';"
        ),
    )
    .await;

    let path = temp_file("pg-table.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"id\":1,\"name\":\"甲\",\"qty\":1,\"note\":\"a'b\\\\c\"}\n",
            "{\"id\":2,\"name\":\"乙\"}\n",
            "{\"id\":3,\"name\":\"丙\",\"ghost\":true}\n",
            "not json\n",
            "{\"just\":\"unknown\"}\n",
        ),
    )
    .expect("写测试 jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let count_sql = format!("SELECT COUNT(*) FROM \"{schema}\".\"items\";");

    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (schema, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("首次导入失败");
    assert_eq!(summary.items, 3, "警告：{:?}", summary.warnings);
    assert_eq!(summary.failed, 2);
    assert!(summary.warnings.iter().any(|w| w.contains("ghost")));
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);
    // PG 标准串不吃反斜杠转义：文本应原样保真
    match scalar_value(
        &svc,
        &config,
        &format!("SELECT \"note\" FROM \"{schema}\".\"items\" WHERE \"id\" = 1;"),
    )
    .await
    {
        ramag_domain::entities::Value::Text(text) => assert_eq!(text, "a'b\\c"),
        other => panic!("期望文本 note，实得 {other:?}"),
    }
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT \"qty\" FROM \"{schema}\".\"items\" WHERE \"id\" = 2;")
        )
        .await,
        7
    );

    // Skip 幂等：全部冲突跳过
    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (schema, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.items, 0);
    assert_eq!(summary.skipped, 3);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);

    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (schema, "items"),
        &path,
        ConflictPolicy::Overwrite,
        &cancel,
        &progress,
    )
    .await
    .expect("覆盖导入失败");
    assert_eq!(summary.items, 3);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);

    let failed = transfer::import_jsonl_into_table(
        &svc,
        &config,
        (schema, "items"),
        &path,
        ConflictPolicy::Fail,
        &cancel,
        &progress,
    )
    .await;
    assert!(failed.is_err(), "Fail 策略应在冲突时报错");

    exec(
        &svc,
        &config,
        format!("UPDATE \"{schema}\".\"items\" SET \"payload\" = '\\xFF00' WHERE \"id\" = 1;"),
    )
    .await;
    let export_path = temp_file("pg-table-export.sql");
    let summary = transfer::export_sql_table(
        &svc,
        &config,
        (schema, "items"),
        &export_path,
        &cancel,
        &progress,
    )
    .await
    .expect("表导出失败");
    assert_eq!((summary.objects, summary.items), (1, 3));
    let exported = std::fs::read_to_string(&export_path).expect("读取单表 SQL 导出文件");
    assert!(exported.contains(&format!("CREATE TABLE \"{schema}\".\"items\"")));
    assert!(exported.contains(&format!("INSERT INTO \"{schema}\".\"items\"")));
    assert!(exported.contains("CREATE TYPE"));
    assert!(exported.contains("item_state"));
    assert!(!exported.contains("unused_state"));
    assert!(exported.contains("items_name_idx"));
    assert!(exported.contains("single export"));
    assert!(exported.contains("REFERENCES"));
    assert!(exported.contains("unrelated"));
    assert!(!exported.contains(&format!("CREATE TABLE \"{schema}\".\"unrelated\"")));
    let summary = transfer::import_sql_table(
        &svc,
        &config,
        &export_path,
        schema,
        ConflictPolicy::Overwrite,
        &cancel,
        &progress,
    )
    .await
    .expect("表导出文件回灌失败");
    assert_eq!(summary.objects, 1);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);
    assert_eq!(scalar_i64(&svc, &config, &count_sql).await, 3);
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!(
                "SELECT COUNT(*) FROM pg_indexes WHERE schemaname = '{schema}' \
                 AND tablename = 'items' AND indexname = 'items_name_idx';"
            ),
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!(
                "SELECT COUNT(*) FROM pg_constraint con \
                 JOIN pg_class rel ON rel.oid = con.conrelid \
                 JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
                 WHERE ns.nspname = '{schema}' AND rel.relname = 'items' \
                   AND con.contype = 'f';"
            ),
        )
        .await,
        1
    );
    let payload = scalar_value(
        &svc,
        &config,
        &format!("SELECT \"payload\" FROM \"{schema}\".\"items\" WHERE \"id\" = 1;"),
    )
    .await;
    assert!(
        matches!(payload, ramag_domain::entities::Value::Bytes(bytes) if bytes == vec![0xff, 0x00])
    );
    exec(
        &svc,
        &config,
        format!("INSERT INTO \"{schema}\".\"items\" (\"name\") VALUES ('序列恢复');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT \"id\" FROM \"{schema}\".\"items\" WHERE \"name\" = '序列恢复';")
        )
        .await,
        4
    );

    exec(&svc, &config, format!("DROP SCHEMA \"{schema}\" CASCADE;")).await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&export_path);
}

/// 集合级裸 JSONL 导入：键入库 / 脏行计失败 / Skip 幂等（无 _id 文档重复插入）/
/// Overwrite 清空重建 / Fail 遇重复即停
#[tokio::test(flavor = "multi_thread")]
async fn mongo_jsonl_collection_import() {
    let Some(config) = mongo_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MONGO_* 环境变量后运行");
        return;
    };
    let svc = MongoService::new(
        Arc::new(ramag_infra_mongodb::MongoDriver::new()),
        Arc::new(StubStorage),
    );
    let db = "ramag_e2e_jsonl_coll";
    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;

    let path = temp_file("mongo-coll.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"_id\":1,\"name\":\"甲\",\"tags\":[\"a\"]}\n",
            "{\"_id\":2,\"name\":\"乙\",\"when\":{\"$date\":\"2026-01-02T03:04:05Z\"}}\n",
            "{\"name\":\"丙\"}\n",
            "not json\n",
            "[1,2]\n",
        ),
    )
    .expect("写测试 jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();

    // 首次导入：3 条入库、2 行脏数据计失败
    let summary = transfer::import_jsonl_into_collection(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("首次导入失败");
    assert_eq!(summary.items, 3, "警告：{:?}", summary.warnings);
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.skipped, 0);
    assert_eq!(
        svc.count(&config, db, "items", &Value::Null).await.unwrap(),
        3
    );

    // Skip 重复导入：_id 1/2 重复跳过，无 _id 文档以新 ObjectId 再插一条
    let summary = transfer::import_jsonl_into_collection(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.items, 1);
    assert_eq!(summary.skipped, 2);
    assert_eq!(
        svc.count(&config, db, "items", &Value::Null).await.unwrap(),
        4
    );

    // Overwrite：先清空集合文档再导入
    let summary = transfer::import_jsonl_into_collection(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Overwrite,
        &cancel,
        &progress,
    )
    .await
    .expect("覆盖导入失败");
    assert_eq!(summary.items, 3);
    assert_eq!(
        svc.count(&config, db, "items", &Value::Null).await.unwrap(),
        3
    );

    // Fail：严格插入遇重复 _id 即报错，集合保持不变
    let failed = transfer::import_jsonl_into_collection(
        &svc,
        &config,
        (db, "items"),
        &path,
        ConflictPolicy::Fail,
        &cancel,
        &progress,
    )
    .await;
    assert!(failed.is_err(), "Fail 策略应在重复 _id 时报错");
    assert_eq!(
        svc.count(&config, db, "items", &Value::Null).await.unwrap(),
        3
    );

    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;
    let _ = std::fs::remove_file(&path);
}
