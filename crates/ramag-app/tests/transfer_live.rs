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
    QueryRecordId, RedisValue, StreamEntry, TransferProgress, TransferSummary, ValuePageCursor,
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
    let user = std::env::var("RAMAG_TEST_MYSQL_USER").ok()?;
    let password = std::env::var("RAMAG_TEST_MYSQL_PASSWORD").ok()?;
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

// ===== MySQL =====

#[tokio::test(flavor = "multi_thread")]
async fn mysql_export_import_roundtrip() {
    let Some(config) = mysql_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_MYSQL_* 环境变量后运行");
        return;
    };
    let svc = sql_service();
    let db = "ramag_e2e_sql";
    let note = "line1\nline2'q\\x";

    exec(&svc, &config, format!("DROP DATABASE IF EXISTS `{db}`;")).await;
    exec(
        &svc,
        &config,
        format!(
            "CREATE DATABASE `{db}`;\n\
             CREATE TABLE `{db}`.`parent` (`id` INT NOT NULL AUTO_INCREMENT PRIMARY KEY, `name` VARCHAR(64) NOT NULL, `note` TEXT NULL, `bin` VARBINARY(8) NULL);\n\
             CREATE TABLE `{db}`.`child` (`id` INT NOT NULL PRIMARY KEY, `pid` INT NOT NULL, CONSTRAINT `fk_child_parent` FOREIGN KEY (`pid`) REFERENCES `{db}`.`parent`(`id`));\n\
             INSERT INTO `{db}`.`parent` (`id`,`name`,`note`,`bin`) VALUES (1,'甲','line1\nline2''q\\\\x',X'FF00'),(2,'乙',NULL,NULL),(3,'C','t;v',NULL);\n\
             INSERT INTO `{db}`.`child` VALUES (10,1),(11,2);\n\
             CREATE VIEW `{db}`.`v_names` AS SELECT `name` FROM `{db}`.`parent`;"
        ),
    )
    .await;

    let path = temp_file("mysql.sql");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let summary = transfer::export_sql_database(&svc, &config, db, &path, &cancel, &progress)
        .await
        .expect("导出失败");
    assert_eq!(summary.objects, 3, "2 表 + 1 视图");
    assert_eq!(summary.items, 5, "共 5 行");
    assert!(!summary.cancelled);

    exec(&svc, &config, format!("DROP DATABASE `{db}`;")).await;

    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Skip,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("导入失败");
    assert_eq!(summary.objects, 3);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);

    // 数据保真：行数、含换行 / 引号 / 反斜杠的文本、二进制、视图
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM `{db}`.`parent`;")
        )
        .await,
        3
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM `{db}`.`child`;")
        )
        .await,
        2
    );
    let got = scalar_value(
        &svc,
        &config,
        &format!("SELECT `note` FROM `{db}`.`parent` WHERE `id` = 1;"),
    )
    .await;
    assert!(
        matches!(&got, ramag_domain::entities::Value::Text(text) if text == note),
        "换行文本往返失真：{got:?}"
    );
    let bin = scalar_value(
        &svc,
        &config,
        &format!("SELECT `bin` FROM `{db}`.`parent` WHERE `id` = 1;"),
    )
    .await;
    assert!(
        matches!(&bin, ramag_domain::entities::Value::Bytes(bytes) if *bytes == vec![0xff, 0x00])
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM `{db}`.`v_names`;")
        )
        .await,
        3
    );

    // AUTO_INCREMENT 续值：新插入应得 id=4
    exec(
        &svc,
        &config,
        format!("INSERT INTO `{db}`.`parent` (`name`) VALUES ('新');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT MAX(`id`) FROM `{db}`.`parent`;")
        )
        .await,
        4
    );

    // 可重复导入：跳过策略全部命中已存在对象，零失败
    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Skip,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.skipped, 3);
    assert_eq!(summary.failed, 0);

    // 合并策略：删行后按条目补回（INSERT IGNORE），已存在行不重复
    exec(
        &svc,
        &config,
        format!("DELETE FROM `{db}`.`child` WHERE `id` = 11;"),
    )
    .await;
    exec(
        &svc,
        &config,
        format!("DELETE FROM `{db}`.`parent` WHERE `id` = 2;"),
    )
    .await;
    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Merge,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("合并导入失败");
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM `{db}`.`parent`;")
        )
        .await,
        4,
        "缺行补回且已有行不重复（1,2 补回,3,4 手动）"
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM `{db}`.`child`;")
        )
        .await,
        2
    );

    exec(&svc, &config, format!("DROP DATABASE `{db}`;")).await;
    let _ = std::fs::remove_file(&path);
}

// ===== PostgreSQL =====

#[tokio::test(flavor = "multi_thread")]
async fn postgres_export_import_roundtrip() {
    let Some(config) = pg_config() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 环境变量后运行");
        return;
    };
    let svc = sql_service();
    let schema = "ramag_e2e_sql";
    let note = "l1\nl2'q\\x";

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
             CREATE TYPE \"{schema}\".\"mood\" AS ENUM ('happy','sad');\n\
             CREATE TABLE \"{schema}\".\"orders\" (\
               \"id\" serial PRIMARY KEY, \
               \"mood\" \"{schema}\".\"mood\" NOT NULL, \
               \"note\" text, \
               \"bin\" bytea, \
               \"price\" numeric NOT NULL DEFAULT 0, \
               \"total\" numeric GENERATED ALWAYS AS (price * 2) STORED);\n\
             CREATE TABLE \"{schema}\".\"items\" (\
               \"iid\" bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
               \"order_id\" int NOT NULL REFERENCES \"{schema}\".\"orders\"(\"id\"), \
               \"qty\" int NOT NULL);\n\
             INSERT INTO \"{schema}\".\"orders\" (\"id\",\"mood\",\"note\",\"bin\",\"price\") VALUES \
               (1,'happy',E'l1\\nl2''q\\\\x','\\xff00',3),(2,'sad',NULL,NULL,5),(3,'happy','plain',NULL,7);\n\
             INSERT INTO \"{schema}\".\"items\" (\"iid\",\"order_id\",\"qty\") OVERRIDING SYSTEM VALUE VALUES (1,1,2),(2,3,4);\n\
             CREATE VIEW \"{schema}\".\"v_orders\" AS SELECT \"id\",\"mood\" FROM \"{schema}\".\"orders\";"
        ),
    )
    .await;

    let path = temp_file("pg.sql");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let summary = transfer::export_sql_database(&svc, &config, schema, &path, &cancel, &progress)
        .await
        .expect("导出失败");
    assert_eq!(summary.objects, 3, "2 表 + 1 视图");
    assert_eq!(summary.items, 5);

    exec(&svc, &config, format!("DROP SCHEMA \"{schema}\" CASCADE;")).await;

    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Skip,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("导入失败");
    assert_eq!(summary.objects, 3);
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);

    // 数据保真：行数、E'' 换行文本、bytea、枚举、生成列
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"orders\";")
        )
        .await,
        3
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"items\";")
        )
        .await,
        2
    );
    let got = scalar_value(
        &svc,
        &config,
        &format!("SELECT \"note\" FROM \"{schema}\".\"orders\" WHERE \"id\" = 1;"),
    )
    .await;
    assert!(
        matches!(&got, ramag_domain::entities::Value::Text(text) if text == note),
        "换行文本往返失真：{got:?}"
    );
    let bin = scalar_value(
        &svc,
        &config,
        &format!("SELECT \"bin\" FROM \"{schema}\".\"orders\" WHERE \"id\" = 1;"),
    )
    .await;
    assert!(
        matches!(&bin, ramag_domain::entities::Value::Bytes(bytes) if *bytes == vec![0xff, 0x00])
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"orders\" WHERE \"mood\" = 'happy';")
        )
        .await,
        2,
        "枚举类型应随导出重建"
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!(
                "SELECT COUNT(*) FROM \"{schema}\".\"orders\" WHERE \"total\" = \"price\" * 2;"
            )
        )
        .await,
        3,
        "生成列应重算一致"
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"v_orders\";")
        )
        .await,
        3
    );

    // 序列续值：serial 与 identity 都应从 MAX+1 继续（缺 setval 会主键冲突）
    exec(
        &svc,
        &config,
        format!("INSERT INTO \"{schema}\".\"orders\" (\"mood\",\"price\") VALUES ('sad',9);"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT MAX(\"id\") FROM \"{schema}\".\"orders\";")
        )
        .await,
        4
    );
    exec(
        &svc,
        &config,
        format!("INSERT INTO \"{schema}\".\"items\" (\"order_id\",\"qty\") VALUES (1,9);"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT MAX(\"iid\") FROM \"{schema}\".\"items\";")
        )
        .await,
        3
    );

    // 可重复导入
    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Skip,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("重复导入失败");
    assert_eq!(summary.skipped, 3);
    assert_eq!(summary.failed, 0);

    // 合并策略：删行后按条目补回（ON CONFLICT DO NOTHING），已存在行不重复
    exec(
        &svc,
        &config,
        format!("DELETE FROM \"{schema}\".\"items\" WHERE \"iid\" = 2;"),
    )
    .await;
    let summary = transfer::import_sql_database(
        &svc,
        &config,
        &path,
        ConflictPolicy::Merge,
        None,
        &cancel,
        &progress,
    )
    .await
    .expect("合并导入失败");
    assert_eq!(summary.failed, 0, "警告：{:?}", summary.warnings);
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"items\";")
        )
        .await,
        3,
        "iid=2 补回，1 与手动 3 不重复"
    );
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"orders\";")
        )
        .await,
        4
    );

    exec(&svc, &config, format!("DROP SCHEMA \"{schema}\" CASCADE;")).await;
    let _ = std::fs::remove_file(&path);
}

// ===== Redis =====

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

    flush_db(&svc, &config, db).await;
    let _ = std::fs::remove_file(&path);
}

async fn flush_db(svc: &RedisService, config: &ConnectionConfig, db: u8) {
    let _ = svc
        .execute_command(config, db, vec!["FLUSHDB".into()])
        .await;
}

// ===== MongoDB =====

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

    let _ = svc
        .run_command(&config, db, json!({"dropDatabase": 1}))
        .await;
    let _ = std::fs::remove_file(&path);
}

/// 汇总结构可用于断言（编译期防字段误删）
#[allow(dead_code)]
fn assert_summary_shape(summary: &TransferSummary) -> (u64, u64) {
    (summary.objects, summary.items)
}

/// 性能探针：对种子库做只读导出（MySQL / PG / Mongo），Redis 走 db0 导出 → db14 导入、
/// Mongo 另导入临时库后清理。仅 RAMAG_TEST_DATASET=full 时运行，输出需 --nocapture。
/// 不写任何种子库
#[tokio::test(flavor = "multi_thread")]
async fn perf_probe_seeded_exports() {
    if std::env::var("RAMAG_TEST_DATASET").as_deref() != Ok("full") {
        eprintln!("[SKIP] RAMAG_TEST_DATASET=full 时才运行性能探针");
        return;
    }
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let mib = |bytes: u64| bytes as f64 / 1048576.0;

    if let Some(config) = mysql_config() {
        let db = std::env::var("RAMAG_TEST_MYSQL_DB").unwrap_or_else(|_| "ramag_test".into());
        let svc = sql_service();
        let path = temp_file("perf-mysql.sql");
        let summary = transfer::export_sql_database(&svc, &config, &db, &path, &cancel, &progress)
            .await
            .expect("mysql 导出失败");
        eprintln!(
            "[PERF] MySQL 导出 {db}：{} 行 / {:.1} MiB / {} ms",
            summary.items,
            mib(summary.bytes),
            summary.elapsed_ms
        );
        let _ = std::fs::remove_file(&path);
    }

    if let Some(config) = pg_config() {
        let svc = sql_service();
        let path = temp_file("perf-pg.sql");
        let summary =
            transfer::export_sql_database(&svc, &config, "public", &path, &cancel, &progress)
                .await
                .expect("pg 导出失败");
        eprintln!(
            "[PERF] PG 导出 public：{} 行 / {:.1} MiB / {} ms",
            summary.items,
            mib(summary.bytes),
            summary.elapsed_ms
        );
        let _ = std::fs::remove_file(&path);
    }

    if let Some(config) = mongo_config() {
        let db = std::env::var("RAMAG_TEST_MONGO_DB").unwrap_or_else(|_| "ramag_demo".into());
        let svc = MongoService::new(
            Arc::new(ramag_infra_mongodb::MongoDriver::new()),
            Arc::new(StubStorage),
        );
        let path = temp_file("perf-mongo.jsonl");
        let summary =
            transfer::export_mongo_database(&svc, &config, &db, &path, &cancel, &progress)
                .await
                .expect("mongo 导出失败");
        eprintln!(
            "[PERF] Mongo 导出 {db}：{} 文档 / {:.1} MiB / {} ms",
            summary.items,
            mib(summary.bytes),
            summary.elapsed_ms
        );
        let scratch = "ramag_perf_import";
        let _ = svc
            .run_command(&config, scratch, json!({"dropDatabase": 1}))
            .await;
        let summary = transfer::import_mongo_database(
            &svc,
            &config,
            &path,
            Some(scratch),
            ConflictPolicy::Skip,
            &cancel,
            &progress,
        )
        .await
        .expect("mongo 导入失败");
        eprintln!(
            "[PERF] Mongo 导入 {scratch}：{} 文档 / {} ms",
            summary.items, summary.elapsed_ms
        );
        let _ = svc
            .run_command(&config, scratch, json!({"dropDatabase": 1}))
            .await;
        let _ = std::fs::remove_file(&path);
    }

    if let Some(config) = redis_config() {
        let svc = RedisService::new(
            Arc::new(ramag_infra_redis::RedisDriver::new()),
            Arc::new(StubStorage),
        );
        let path = temp_file("perf-redis.jsonl");
        let summary = transfer::export_redis_db(&svc, &config, 0, &path, &cancel, &progress)
            .await
            .expect("redis 导出失败");
        eprintln!(
            "[PERF] Redis 导出 db0：{} key / {} 条目 / {:.1} MiB / {} ms",
            summary.objects,
            summary.items,
            mib(summary.bytes),
            summary.elapsed_ms
        );
        flush_db(&svc, &config, 14).await;
        let summary = transfer::import_redis_db(
            &svc,
            &config,
            Some(14),
            &path,
            ConflictPolicy::Skip,
            &cancel,
            &progress,
        )
        .await
        .expect("redis 导入失败");
        eprintln!(
            "[PERF] Redis 导入 db14：{} key / {} ms",
            summary.objects, summary.elapsed_ms
        );
        flush_db(&svc, &config, 14).await;
        let _ = std::fs::remove_file(&path);
    }
}

// ===== 表级 JSONL 导入 =====

/// 覆盖：键名匹配 / 缺列走默认与自增 / 未知键告警 / 脏行计失败 /
/// Skip 幂等 / Overwrite 重建 / Fail 冲突即停
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
             `note` TEXT NULL);"
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

    exec(&svc, &config, format!("DROP DATABASE `{db}`;")).await;
    let _ = std::fs::remove_file(&path);
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
             CREATE TABLE \"{schema}\".\"items\" (\
             \"id\" INT PRIMARY KEY, \
             \"name\" TEXT NOT NULL, \
             \"qty\" INT DEFAULT 7, \
             \"note\" TEXT);"
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

    exec(&svc, &config, format!("DROP SCHEMA \"{schema}\" CASCADE;")).await;
    let _ = std::fs::remove_file(&path);
}
