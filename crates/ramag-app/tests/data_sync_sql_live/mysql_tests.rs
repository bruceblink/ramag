//! SQL 数据同步集成测试分组。

use super::*;

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
