#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! 集成测试：连接真实 Redis。缺 RAMAG_TEST_REDIS_HOST/PORT 时跳过。
//! 用 db 15 避免污染 0 号库；测试尾 FLUSHDB 清场

use std::collections::HashSet;

use ramag_domain::entities::{
    ConnectionConfig, MAX_REDIS_COLLECTION_BYTES, MAX_REDIS_LOADED_ITEMS, RedisType, RedisValue,
    StreamEntry, ValuePageCursor,
};
use ramag_domain::traits::KvDriver;
use ramag_infra_redis::RedisDriver;

const TEST_DB: u8 = 15;
const SEEDED_STRING_BYTES: usize = 8 * 1024 * 1024;
const _: () = assert!(SEEDED_STRING_BYTES < MAX_REDIS_COLLECTION_BYTES);

/// 缺 host/port 就跳过测试
fn config_from_env() -> Option<ConnectionConfig> {
    let host = std::env::var("RAMAG_TEST_REDIS_HOST").ok()?;
    let port: u16 = std::env::var("RAMAG_TEST_REDIS_PORT").ok()?.parse().ok()?;
    let password = std::env::var("RAMAG_TEST_REDIS_PASSWORD").unwrap_or_default();
    let username = std::env::var("RAMAG_TEST_REDIS_USERNAME").unwrap_or_default();

    Some(ConnectionConfig {
        username,
        password,
        ..ConnectionConfig::new_redis("redis-integration-test", host, port)
    })
}

fn seeded_dataset_enabled() -> bool {
    std::env::var("RAMAG_TEST_DATASET").as_deref() == Ok("full")
}

macro_rules! require_env {
    () => {{
        match config_from_env() {
            Some(c) => c,
            None => {
                eprintln!(
                    "[SKIP] integration test skipped: 设置 RAMAG_TEST_REDIS_HOST/PORT 环境变量后运行"
                );
                return;
            }
        }
    }};
}

async fn cleanup(driver: &RedisDriver, config: &ConnectionConfig) {
    let _ = driver
        .execute_command(config, TEST_DB, vec!["FLUSHDB".into()])
        .await;
}

#[path = "integration/scan_tests.rs"]
mod scan_tests;
#[path = "integration/value_tests.rs"]
mod value_tests;
