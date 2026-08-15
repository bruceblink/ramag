use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn scan_iterates_full_keyspace() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    for i in 0..30 {
        driver
            .execute_command(
                &config,
                TEST_DB,
                vec!["SET".into(), format!("scan:{i}"), "v".into()],
            )
            .await
            .unwrap();
    }

    let mut cursor = 0u64;
    let mut total = 0;
    loop {
        let r = driver
            .scan(&config, TEST_DB, cursor, Some("scan:*"), None, 10)
            .await
            .unwrap();
        total += r.keys.len();
        cursor = r.cursor;
        if cursor == 0 {
            break;
        }
    }
    assert_eq!(total, 30);

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn key_type_pipeline_preserves_input_order() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["SET".into(), "types:string".into(), "v".into()],
        )
        .await
        .unwrap();
    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "HSET".into(),
                "types:hash".into(),
                "field".into(),
                "v".into(),
            ],
        )
        .await
        .unwrap();
    driver
        .execute_command(
            &config,
            TEST_DB,
            vec![
                "ZADD".into(),
                "types:zset".into(),
                "1".into(),
                "member".into(),
            ],
        )
        .await
        .unwrap();

    let keys = vec![
        "types:zset".to_string(),
        "types:string".to_string(),
        "types:hash".to_string(),
    ];
    let types = driver.key_types(&config, TEST_DB, &keys).await.unwrap();
    assert_eq!(
        types,
        vec![RedisType::ZSet, RedisType::String, RedisType::Hash]
    );

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ttl_set_and_persist() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["SET".into(), "ttl_key".into(), "v".into()],
        )
        .await
        .unwrap();

    // 初始无 TTL
    let ttl = driver.key_ttl(&config, TEST_DB, "ttl_key").await.unwrap();
    assert_eq!(ttl, -1, "无 TTL 应返回 -1");

    let ok = driver
        .set_ttl(&config, TEST_DB, "ttl_key", Some(600))
        .await
        .unwrap();
    assert!(ok);

    let ttl_ms = driver.key_ttl(&config, TEST_DB, "ttl_key").await.unwrap();
    assert!(ttl_ms > 0 && ttl_ms <= 600_000);

    let ok = driver
        .set_ttl(&config, TEST_DB, "ttl_key", None)
        .await
        .unwrap();
    assert!(ok);

    let ttl = driver.key_ttl(&config, TEST_DB, "ttl_key").await.unwrap();
    assert_eq!(ttl, -1);

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_returns_correct_bool() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["SET".into(), "del_target".into(), "v".into()],
        )
        .await
        .unwrap();

    let r = driver
        .delete_key(&config, TEST_DB, "del_target")
        .await
        .unwrap();
    assert!(r, "存在的 key 删除应返回 true");

    let r = driver
        .delete_key(&config, TEST_DB, "del_target")
        .await
        .unwrap();
    assert!(!r, "不存在的 key 删除应返回 false");

    cleanup(&driver, &config).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_key_returns_nil() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    let v = driver
        .get_value(&config, TEST_DB, "definitely_missing")
        .await
        .unwrap();
    assert!(matches!(v, RedisValue::Nil));

    let t = driver
        .key_type(&config, TEST_DB, "definitely_missing")
        .await
        .unwrap();
    assert_eq!(t, RedisType::None);
}

#[tokio::test(flavor = "multi_thread")]
async fn seeded_dataset_scans_full_keyspace_and_applies_current_value_limits() {
    let config = require_env!();
    if !seeded_dataset_enabled() {
        eprintln!("[SKIP] seeded dataset test skipped: RAMAG_TEST_DATASET != full");
        return;
    }
    let driver = RedisDriver::new();
    let expected = driver.db_size(&config, 0).await.unwrap();
    assert!(expected >= 46_000);

    let mut cursor = 0;
    let mut keys = HashSet::new();
    loop {
        let result = driver
            .scan(&config, 0, cursor, None, None, 1000)
            .await
            .unwrap();
        keys.extend(result.keys.into_iter().map(|key| key.key));
        cursor = result.cursor;
        if cursor == 0 {
            break;
        }
    }
    assert_eq!(keys.len() as u64, expected);

    let string = driver
        .get_value_limited(&config, 0, "large:string", 100)
        .await
        .unwrap();
    assert_eq!(string.total, Some(SEEDED_STRING_BYTES as u64));
    assert_eq!(string.loaded_len(), Some(SEEDED_STRING_BYTES));
    assert!(!string.has_more());
    assert!(!string.byte_limited);

    let list = driver
        .get_value_limited(&config, 0, "large:list", 100)
        .await
        .unwrap();
    assert_eq!(list.total, Some(20_000));
    assert_eq!(list.loaded_len(), Some(100));
    assert!(list.has_more());
    assert!(!list.byte_limited);
}

#[tokio::test(flavor = "multi_thread")]
async fn value_page_roundtrip_and_pagination() {
    let config = require_env!();
    let driver = RedisDriver::new();
    cleanup(&driver, &config).await;

    // 6 类型造数：list 1200 条驱动跨页（页 500 → 3 页）
    driver
        .write_value_items(
            &config,
            TEST_DB,
            "vp:text",
            &RedisValue::Text("hello 世界".into()),
        )
        .await
        .expect("写 string 失败");
    driver
        .write_value_items(
            &config,
            TEST_DB,
            "vp:bin",
            &RedisValue::Bytes(vec![0xff, 0x00, 0xfe]),
        )
        .await
        .expect("写二进制 string 失败");
    let members: Vec<RedisValue> = (0..1200)
        .map(|i| RedisValue::Text(format!("m{i:04}")))
        .collect();
    driver
        .write_value_items(&config, TEST_DB, "vp:list", &RedisValue::List(members))
        .await
        .expect("写 list 失败");
    driver
        .write_value_items(
            &config,
            TEST_DB,
            "vp:hash",
            &RedisValue::Hash(vec![
                ("f1".to_string(), RedisValue::Text("v1".into())),
                ("f2".to_string(), RedisValue::Bytes(vec![0xff, 1, 2])),
            ]),
        )
        .await
        .expect("写 hash 失败");
    driver
        .write_value_items(
            &config,
            TEST_DB,
            "vp:zset",
            &RedisValue::ZSet(vec![
                (RedisValue::Text("alice".into()), 1.5),
                (RedisValue::Text("bob".into()), 2.5),
            ]),
        )
        .await
        .expect("写 zset 失败");
    driver
        .write_value_items(
            &config,
            TEST_DB,
            "vp:stream",
            &RedisValue::Stream(vec![
                StreamEntry {
                    id: "1-1".into(),
                    fields: vec![("k".into(), "v".into())],
                },
                StreamEntry {
                    id: "2-1".into(),
                    fields: vec![("k2".into(), "v2".into())],
                },
            ]),
        )
        .await
        .expect("写 stream 失败");

    // 首页 kind=None：单次调用带回类型探测 + PTTL
    let first = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:list",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读 list 首页失败");
    assert!(matches!(first.items, RedisValue::List(_)));
    assert_eq!(first.ttl_ms, Some(-1));

    // 跨页读完整 list，同时逐页写入拷贝（RPUSH 追加语义保持顺序）
    let mut names: Vec<String> = Vec::new();
    let mut page = first;
    loop {
        if let RedisValue::List(items) = &page.items {
            for item in items {
                match item {
                    RedisValue::Text(text) => names.push(text.clone()),
                    other => panic!("非文本成员：{other:?}"),
                }
            }
        }
        driver
            .write_value_items(&config, TEST_DB, "copy:list", &page.items)
            .await
            .expect("写 list 拷贝失败");
        match page.next.clone() {
            Some(next) => {
                page = driver
                    .read_value_page(
                        &config,
                        TEST_DB,
                        "vp:list",
                        Some(RedisType::List),
                        next,
                        500,
                    )
                    .await
                    .expect("读 list 续页失败");
            }
            None => break,
        }
    }
    assert_eq!(names.len(), 1200);
    assert_eq!(names.first().map(String::as_str), Some("m0000"));
    assert_eq!(names.last().map(String::as_str), Some("m1199"));
    let llen = driver
        .execute_command(&config, TEST_DB, vec!["LLEN".into(), "copy:list".into()])
        .await
        .expect("LLEN 失败");
    assert!(matches!(llen, RedisValue::Int(1200)));

    // 二进制 string / hash 值保真
    let bin = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:bin",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读二进制 string 失败");
    assert!(matches!(bin.items, RedisValue::Bytes(bytes) if bytes == vec![0xff, 0x00, 0xfe]));
    let hash = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:hash",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读 hash 失败");
    match &hash.items {
        RedisValue::Hash(pairs) => {
            assert_eq!(pairs.len(), 2);
            assert!(pairs.iter().any(|(field, value)| field == "f2"
                && matches!(value, RedisValue::Bytes(bytes) if *bytes == vec![0xff, 1, 2])));
        }
        other => panic!("期望 Hash：{other:?}"),
    }

    // zset / stream 往返
    let zset = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:zset",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读 zset 失败");
    driver
        .write_value_items(&config, TEST_DB, "copy:zset", &zset.items)
        .await
        .expect("写 zset 拷贝失败");
    let score = driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["ZSCORE".into(), "copy:zset".into(), "bob".into()],
        )
        .await
        .expect("ZSCORE 失败");
    match score {
        RedisValue::Text(text) => assert_eq!(text, "2.5"),
        RedisValue::Float(value) => assert!((value - 2.5).abs() < 1e-9),
        other => panic!("ZSCORE 应答异常：{other:?}"),
    }
    let stream = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:stream",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读 stream 失败");
    match &stream.items {
        RedisValue::Stream(entries) => assert_eq!(entries.len(), 2),
        other => panic!("期望 Stream：{other:?}"),
    }
    driver
        .write_value_items(&config, TEST_DB, "copy:stream", &stream.items)
        .await
        .expect("写 stream 拷贝失败");
    let xlen = driver
        .execute_command(&config, TEST_DB, vec!["XLEN".into(), "copy:stream".into()])
        .await
        .expect("XLEN 失败");
    assert!(matches!(xlen, RedisValue::Int(2)));

    // TTL 探测（PEXPIRE 后首页 ttl_ms > 0）与不存在 key（Nil + -2）
    driver
        .execute_command(
            &config,
            TEST_DB,
            vec!["PEXPIRE".into(), "vp:text".into(), "60000".into()],
        )
        .await
        .expect("PEXPIRE 失败");
    let ttl_page = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:text",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读带 TTL key 失败");
    assert!(ttl_page.ttl_ms.is_some_and(|ttl| ttl > 0));
    let gone = driver
        .read_value_page(
            &config,
            TEST_DB,
            "vp:missing",
            None,
            ValuePageCursor::Start,
            500,
        )
        .await
        .expect("读不存在 key 失败");
    assert!(matches!(gone.items, RedisValue::Nil));
    assert_eq!(gone.ttl_ms, Some(-2));

    // 批量首页保持输入顺序、类型与 TTL；导出依赖该顺序写出稳定 JSONL。
    let batch_keys = [
        "vp:text",
        "vp:bin",
        "vp:list",
        "vp:hash",
        "vp:zset",
        "vp:stream",
        "vp:missing",
    ]
    .map(str::to_string);
    let pages = driver
        .read_value_first_pages(&config, TEST_DB, &batch_keys, 500)
        .await
        .expect("批量读取首页失败");
    assert_eq!(pages.len(), batch_keys.len());
    assert!(matches!(pages[0].items, RedisValue::Text(_)));
    assert!(pages[0].ttl_ms.is_some_and(|ttl| ttl > 0));
    assert!(matches!(pages[1].items, RedisValue::Bytes(_)));
    assert!(matches!(pages[2].items, RedisValue::List(_)));
    assert!(matches!(pages[3].items, RedisValue::Hash(_)));
    assert!(matches!(pages[4].items, RedisValue::ZSet(_)));
    assert!(matches!(pages[5].items, RedisValue::Stream(_)));
    assert!(matches!(pages[6].items, RedisValue::Nil));
    assert_eq!(pages[6].ttl_ms, Some(-2));

    // 生产（只读）模式拦截导入写
    let mut readonly = config.clone();
    readonly.production = true;
    let blocked = driver
        .write_value_items(&readonly, TEST_DB, "vp:text", &RedisValue::Text("x".into()))
        .await;
    assert!(blocked.is_err());

    cleanup(&driver, &config).await;
}
