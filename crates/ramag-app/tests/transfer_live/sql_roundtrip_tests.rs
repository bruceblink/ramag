use super::*;

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
