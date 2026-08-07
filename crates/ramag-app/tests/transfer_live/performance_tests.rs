//! 传输性能探针。

use super::*;

/// 对种子库做只读导出；仅在完整测试数据集下运行。
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
