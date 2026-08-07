//! Redis 集成测试分组。

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn test_connection_works() {
    let config = require_env!();
    let driver = RedisDriver::new();
    driver
        .test_connection(&config)
        .await
        .expect("test_connection 失败");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_version_returns_value() {
    let config = require_env!();
    let driver = RedisDriver::new();
    let v = driver
        .server_version(&config)
        .await
        .expect("server_version 失败");
    println!("redis_version: {v}");
    assert!(!v.is_empty());
    assert_ne!(v, "unknown");
}

#[tokio::test(flavor = "multi_thread")]
async fn db_size_and_dbsize_command_match() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    let n0 = driver.db_size(&config, TEST_DB).await.unwrap();
    assert_eq!(n0, 0, "FLUSHDB 后应为 0");

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["SET".into(), "ping_key".into(), "ok".into()],
        )
        .await
        .unwrap();

    let n1 = driver.db_size(&config, TEST_DB).await.unwrap();
    assert_eq!(n1, 1);

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn string_get_set_roundtrip() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["SET".into(), "greet".into(), "hello".into()],
        )
        .await
        .unwrap();

    let v = driver.get_value(&config, TEST_DB, "greet").await.unwrap();
    assert!(matches!(v, RedisValue::Text(s) if s == "hello"));

    let t = driver.key_type(&config, TEST_DB, "greet").await.unwrap();
    assert_eq!(t, RedisType::String);

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_megabyte_string_is_not_truncated_at_the_old_four_mib_limit() {
    const OLD_STRING_PREFIX_LIMIT: usize = 4 * 1024 * 1024;

    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;
    let total = (OLD_STRING_PREFIX_LIMIT - 1 + "界".len()) as u64;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "EVAL".into(),
                "return redis.call('SET', KEYS[1], string.rep('a', tonumber(ARGV[1])) .. '界')"
                    .into(),
                "1".into(),
                "large-text".into(),
                (OLD_STRING_PREFIX_LIMIT - 1).to_string(),
            ],
        )
        .await
        .unwrap();

    let load = driver
        .get_value_limited(&config, TEST_DB, "large-text", 100)
        .await
        .unwrap();
    assert_eq!(load.total, Some(total));
    assert!(!load.has_more());
    assert!(!load.memory_warning);
    assert!(matches!(load.value, RedisValue::Text(text) if text.len() == total as usize));

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_value_returns_pairs() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "HSET".into(),
                "user:1".into(),
                "name".into(),
                "alice".into(),
                "age".into(),
                "30".into(),
            ],
        )
        .await
        .unwrap();

    let v = driver.get_value(&config, TEST_DB, "user:1").await.unwrap();
    match v {
        RedisValue::Hash(pairs) => {
            assert_eq!(pairs.len(), 2);
            let names: Vec<_> = pairs.iter().map(|(k, _)| k.as_str()).collect();
            assert!(names.contains(&"name"));
            assert!(names.contains(&"age"));
        }
        other => panic!("期望 Hash，实得 {other:?}"),
    }

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_detail_loads_all_members_up_to_global_limit() {
    const MEMBER_COUNT: usize = 20_000;

    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;
    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "EVAL".into(),
                "for i = 1, tonumber(ARGV[1]) do redis.call('HSET', KEYS[1], 'field:' .. i, 'value:' .. i) end return redis.call('HLEN', KEYS[1])".into(),
                "1".into(),
                "large:hash".into(),
                MEMBER_COUNT.to_string(),
            ],
        )
        .await
        .unwrap();

    let load = driver
        .get_value_limited(&config, TEST_DB, "large:hash", MAX_REDIS_LOADED_ITEMS)
        .await
        .unwrap();

    assert_eq!(load.total, Some(MEMBER_COUNT as u64));
    assert_eq!(load.loaded_len(), Some(MEMBER_COUNT));
    assert!(!load.has_more());
    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_value_preserves_order() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "RPUSH".into(),
                "l".into(),
                "a".into(),
                "b".into(),
                "c".into(),
            ],
        )
        .await
        .unwrap();

    let v = driver.get_value(&config, TEST_DB, "l").await.unwrap();
    match v {
        RedisValue::List(elems) => {
            assert_eq!(elems.len(), 3);
            assert!(matches!(&elems[0], RedisValue::Text(s) if s == "a"));
            assert!(matches!(&elems[2], RedisValue::Text(s) if s == "c"));
        }
        other => panic!("期望 List，实得 {other:?}"),
    }

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zset_value_with_scores() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "ZADD".into(),
                "scores".into(),
                "1.5".into(),
                "alice".into(),
                "2.5".into(),
                "bob".into(),
            ],
        )
        .await
        .unwrap();

    let v = driver.get_value(&config, TEST_DB, "scores").await.unwrap();
    match v {
        RedisValue::ZSet(pairs) => {
            assert_eq!(pairs.len(), 2);
            assert!((pairs[0].1 - 1.5).abs() < 1e-9);
            assert!((pairs[1].1 - 2.5).abs() < 1e-9);
        }
        other => panic!("期望 ZSet，实得 {other:?}"),
    }

    cleanup(&driver, &config).await;
}
