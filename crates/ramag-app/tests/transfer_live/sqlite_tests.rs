use super::*;

/// 验证 SQLite 的 JSONL 导入、缺省值和 Skip 冲突策略走真实驱动。
#[tokio::test(flavor = "multi_thread")]
async fn sqlite_jsonl_table_import() {
    let svc = sql_service();
    let path = temp_file("sqlite-jsonl.sqlite3");
    let config = ConnectionConfig::new_sqlite("transfer-e2e", path.to_string_lossy().into_owned());
    exec(
        &svc,
        &config,
        "CREATE TABLE \"items\" (\"id\" INTEGER PRIMARY KEY, \"name\" TEXT NOT NULL, \"qty\" INTEGER DEFAULT 7, \"note\" TEXT);",
    )
    .await;

    let jsonl_path = temp_file("sqlite-table.jsonl");
    std::fs::write(
        &jsonl_path,
        concat!(
            "{\"id\":1,\"name\":\"甲\",\"qty\":1,\"note\":\"a'b\\\\c\"}\n",
            "{\"id\":2,\"name\":\"乙\"}\n",
            "{\"name\":\"丙\",\"ghost\":true}\n",
            "not json\n",
        ),
    )
    .expect("写 SQLite 测试 jsonl");
    let cancel = AtomicBool::new(false);
    let progress = noop_progress();
    let count_sql = "SELECT COUNT(*) FROM \"main\".\"items\";";

    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        ("main", "items"),
        &jsonl_path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("SQLite 首次导入失败");
    assert_eq!(summary.items, 3, "警告：{:?}", summary.warnings);
    assert_eq!(summary.failed, 1);
    assert_eq!(scalar_i64(&svc, &config, count_sql).await, 3);
    match scalar_value(
        &svc,
        &config,
        "SELECT \"note\" FROM \"main\".\"items\" WHERE \"id\" = 1;",
    )
    .await
    {
        ramag_domain::entities::Value::Text(text) => assert_eq!(text, "a'b\\c"),
        other => panic!("期望 SQLite 文本 note，实得 {other:?}"),
    }
    assert_eq!(
        scalar_i64(
            &svc,
            &config,
            "SELECT \"qty\" FROM \"main\".\"items\" WHERE \"id\" = 2;",
        )
        .await,
        7
    );

    let summary = transfer::import_jsonl_into_table(
        &svc,
        &config,
        ("main", "items"),
        &jsonl_path,
        ConflictPolicy::Skip,
        &cancel,
        &progress,
    )
    .await
    .expect("SQLite 重复导入失败");
    assert_eq!(summary.items, 1);
    assert_eq!(summary.skipped, 2);
    assert_eq!(scalar_i64(&svc, &config, count_sql).await, 4);

    exec(&svc, &config, "DROP TABLE \"items\";").await;
    let _ = std::fs::remove_file(&jsonl_path);
    let _ = std::fs::remove_file(&path);
}
