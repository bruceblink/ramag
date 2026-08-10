use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn postgres_sync_maps_schema_enum_identity_sequence_and_foreign_key() {
    let Some((source, target)) = postgres_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 后运行 SQL 同步测试");
        return;
    };
    let suffix = std::process::id();
    let source_schema = format!("ramag_sync_pg_src_{suffix}");
    let target_schema = format!("ramag_sync_pg_dst_{suffix}");
    let new_schema = format!("ramag_sync_pg_new_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{new_schema}\" CASCADE; \
             CREATE SCHEMA \"{source_schema}\"; CREATE SCHEMA \"{target_schema}\"; \
             CREATE TYPE \"{source_schema}\".\"status\" AS ENUM ('active','disabled'); \
             CREATE TYPE \"{target_schema}\".\"status\" AS ENUM ('active','disabled'); \
             CREATE TABLE \"{source_schema}\".\"parent\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \"email\" TEXT NOT NULL UNIQUE, \"state\" \"{source_schema}\".\"status\" NOT NULL); \
             CREATE TABLE \"{source_schema}\".\"child\" (\"id\" BIGINT PRIMARY KEY, \"parent_id\" BIGINT NOT NULL REFERENCES \"{source_schema}\".\"parent\"(\"id\")); \
             INSERT INTO \"{source_schema}\".\"parent\" (\"id\",\"email\",\"state\") OVERRIDING SYSTEM VALUE VALUES (1,'one@test','active'),(2,'two@test','disabled'); \
             INSERT INTO \"{source_schema}\".\"child\" VALUES (10,1),(11,2); \
             CREATE TABLE \"{target_schema}\".\"parent_copy\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \"email\" TEXT NOT NULL UNIQUE, \"state\" \"{target_schema}\".\"status\" NOT NULL); \
             INSERT INTO \"{target_schema}\".\"parent_copy\" (\"id\",\"email\",\"state\") OVERRIDING SYSTEM VALUE VALUES (1,'target-kept@test','active');"
        ),
    )
    .await;

    let postgres_scopes = sync
        .list_catalog_scopes(&source)
        .await
        .expect("读取 PostgreSQL Schema 目录");
    assert!(postgres_scopes.contains(&source_schema));
    assert!(!postgres_scopes.contains(&"pg_catalog".to_string()));
    let postgres_catalog = sync
        .list_catalog_objects(&source, &source_schema)
        .await
        .expect("读取 PostgreSQL Table 目录");
    assert!(!postgres_catalog.truncated);
    assert!(postgres_catalog.names.contains(&"parent".to_string()));
    assert!(postgres_catalog.names.contains(&"child".to_string()));

    let request = sql_request(
        &source,
        &target,
        &source_schema,
        &target_schema,
        &[("parent", "parent_copy"), ("child", "child_copy")],
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
            format!("SELECT \"email\" FROM \"{target_schema}\".\"parent_copy\" WHERE \"id\"=1;")
        )
        .await,
        "target-kept@test"
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM \"{target_schema}\".\"child_copy\";")
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT COUNT(*) FROM pg_constraint WHERE conrelid='\"{target_schema}\".\"child_copy\"'::regclass AND contype='f';")
        )
        .await,
        1
    );
    exec(
        &sql,
        &target,
        format!("INSERT INTO \"{target_schema}\".\"parent_copy\" (\"email\",\"state\") VALUES ('next@test','active');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"parent_copy\";")
        )
        .await,
        3
    );

    // 目标序列可能因预分配或历史删除领先于当前最大 ID，同步不得把它倒退。
    exec(
        &sql,
        &target,
        format!(
            "SELECT setval(pg_get_serial_sequence('\"{target_schema}\".\"parent_copy\"', 'id'), 1000, false);"
        ),
    )
    .await;

    let repeat = execute_sync(
        &sync,
        &gate,
        request,
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(repeat.inserted, 0);
    assert_eq!(repeat.skipped, 4);
    exec(
        &sql,
        &target,
        format!(
            "INSERT INTO \"{target_schema}\".\"parent_copy\" (\"email\",\"state\") VALUES ('sequence-ahead@test','active');"
        ),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"parent_copy\";")
        )
        .await,
        1000
    );

    let create_request = sql_request(
        &source,
        &target,
        &source_schema,
        &new_schema,
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
        scalar_text(
            &sql,
            &target,
            format!(
                "SELECT \"state\"::text FROM \"{new_schema}\".\"parent_archive\" WHERE \"id\"=2;"
            )
        )
        .await,
        "disabled"
    );
    exec(
        &sql,
        &target,
        format!("INSERT INTO \"{new_schema}\".\"parent_archive\" (\"email\",\"state\") VALUES ('new-next@test','active');"),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{new_schema}\".\"parent_archive\";")
        )
        .await,
        3
    );

    // 已有目标使用自定义序列名时，应推进目标自己的序列，而不是猜测源序列映射名。
    exec(
        &sql,
        &source,
        format!(
            "CREATE TABLE \"{source_schema}\".\"serial_source\" (\"id\" BIGSERIAL PRIMARY KEY, \"name\" TEXT NOT NULL); \
             INSERT INTO \"{source_schema}\".\"serial_source\" (\"id\",\"name\") VALUES (5,'source-five'); \
             CREATE SEQUENCE \"{target_schema}\".\"custom_serial_sequence\"; \
             CREATE TABLE \"{target_schema}\".\"serial_target\" (\"id\" BIGINT DEFAULT nextval('\"{target_schema}\".\"custom_serial_sequence\"'::regclass) PRIMARY KEY, \"name\" TEXT NOT NULL); \
             ALTER SEQUENCE \"{target_schema}\".\"custom_serial_sequence\" OWNED BY \"{target_schema}\".\"serial_target\".\"id\";"
        ),
    )
    .await;
    let custom_sequence = execute_sync(
        &sync,
        &gate,
        sql_request(
            &source,
            &target,
            &source_schema,
            &target_schema,
            &[("serial_source", "serial_target")],
        ),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(custom_sequence.inserted, 1);
    exec(
        &sql,
        &target,
        format!(
            "INSERT INTO \"{target_schema}\".\"serial_target\" (\"name\") VALUES ('target-next');"
        ),
    )
    .await;
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!("SELECT MAX(\"id\") FROM \"{target_schema}\".\"serial_target\";")
        )
        .await,
        6
    );

    // Identity 模式不同会影响显式值插入，必须在预检阶段拒绝。
    exec(
        &sql,
        &source,
        format!(
            "CREATE TABLE \"{source_schema}\".\"plain_identity\" (\"id\" BIGINT PRIMARY KEY); \
             CREATE TABLE \"{target_schema}\".\"identity_mismatch\" (\"id\" BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY);"
        ),
    )
    .await;
    let identity_error = sync
        .preflight(sql_request(
            &source,
            &target,
            &source_schema,
            &target_schema,
            &[("plain_identity", "identity_mismatch")],
        ))
        .await
        .err()
        .expect("Identity 模式不一致必须拒绝");
    assert!(identity_error.message().contains("Identity 模式"));

    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{target_schema}\" CASCADE; DROP SCHEMA IF EXISTS \"{new_schema}\" CASCADE;"
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_sync_reuses_shared_enum_after_partial_creation_and_rejects_conflict() {
    let Some((source, target)) = postgres_configs() else {
        eprintln!("[SKIP] 设置 RAMAG_TEST_PG_* 后运行 SQL 同步测试");
        return;
    };
    let suffix = std::process::id();
    let source_schema = format!("ramag_sync_enum_src_{suffix}");
    let fresh_schema = format!("ramag_sync_enum_fresh_{suffix}");
    let partial_schema = format!("ramag_sync_enum_partial_{suffix}");
    let conflict_schema = format!("ramag_sync_enum_conflict_{suffix}");
    let stale_schema = format!("ramag_sync_enum_stale_{suffix}");
    let (sync, sql, gate) = services(&source, &target);
    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{fresh_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{partial_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{conflict_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{stale_schema}\" CASCADE; \
             CREATE SCHEMA \"{source_schema}\"; \
             CREATE TYPE \"{source_schema}\".\"record_state\" AS ENUM ('new','active','archived'); \
             CREATE TABLE \"{source_schema}\".\"first_records\" (\
                 \"id\" BIGINT PRIMARY KEY, \
                 \"state\" \"{source_schema}\".\"record_state\" NOT NULL, \
                 \"previous_state\" \"{source_schema}\".\"record_state\" NOT NULL\
             ); \
             CREATE TABLE \"{source_schema}\".\"second_records\" (\
                 \"id\" BIGINT PRIMARY KEY, \
                 \"state\" \"{source_schema}\".\"record_state\" NOT NULL\
             ); \
             INSERT INTO \"{source_schema}\".\"first_records\" VALUES (1,'active','new'); \
             INSERT INTO \"{source_schema}\".\"second_records\" VALUES (2,'archived'); \
             CREATE SCHEMA \"{partial_schema}\"; \
             CREATE TYPE \"{partial_schema}\".\"record_state\" AS ENUM ('new','active','archived'); \
             CREATE SCHEMA \"{conflict_schema}\"; \
             CREATE TYPE \"{conflict_schema}\".\"record_state\" AS ENUM ('new','disabled'); \
             CREATE SCHEMA \"{stale_schema}\";"
        ),
    )
    .await;

    let mappings = &[
        ("first_records", "first_records"),
        ("second_records", "second_records"),
    ];
    let fresh = execute_sync(
        &sync,
        &gate,
        sql_request(&source, &target, &source_schema, &fresh_schema, mappings),
        DataSyncConfirmation::CreateMissingTargets,
    )
    .await;
    assert_eq!(fresh.inserted, 2);
    assert_eq!(
        scalar_i64(
            &sql,
            &target,
            format!(
                "SELECT COUNT(*) FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace \
                 WHERE n.nspname='{fresh_schema}' AND t.typname='record_state';"
            ),
        )
        .await,
        1
    );
    let repeated = execute_sync(
        &sync,
        &gate,
        sql_request(&source, &target, &source_schema, &fresh_schema, mappings),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(repeated.inserted, 0);
    assert_eq!(repeated.skipped, 2);

    let resumed = execute_sync(
        &sync,
        &gate,
        sql_request(&source, &target, &source_schema, &partial_schema, mappings),
        DataSyncConfirmation::ContinueWithExistingTargets,
    )
    .await;
    assert_eq!(resumed.inserted, 2);

    let conflict = sync
        .preflight(sql_request(
            &source,
            &target,
            &source_schema,
            &conflict_schema,
            mappings,
        ))
        .await
        .err()
        .expect("目标枚举定义冲突必须在预检阶段拒绝");
    assert!(conflict.message().contains("record_state"));
    assert!(conflict.message().contains("选项定义与源不一致"));

    let stale = sync
        .preflight(sql_request(
            &source,
            &target,
            &source_schema,
            &stale_schema,
            mappings,
        ))
        .await
        .expect("枚举并发变化前预检应成功");
    exec(
        &sql,
        &target,
        format!(
            "CREATE TYPE \"{stale_schema}\".\"record_state\" AS ENUM ('new','active','archived');"
        ),
    )
    .await;
    let started = sync
        .start(stale, DataSyncConfirmation::ContinueWithExistingTargets)
        .expect("枚举并发变化应在占屏执行期返回");
    let permit = started.permit().clone();
    sync.execute(started).await;
    let snapshot = gate.snapshot().expect("枚举并发变化结果应保持占屏");
    assert_eq!(snapshot.phase, DataSyncGatePhase::Failed);
    assert!(
        snapshot
            .error
            .is_some_and(|error| error.contains("结构已在预检后变化"))
    );
    assert!(sync.acknowledge_result(&permit));

    exec(
        &sql,
        &source,
        format!(
            "DROP SCHEMA IF EXISTS \"{source_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{fresh_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{partial_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{conflict_schema}\" CASCADE; \
             DROP SCHEMA IF EXISTS \"{stale_schema}\" CASCADE;"
        ),
    )
    .await;
}
