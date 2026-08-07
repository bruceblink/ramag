//! 拆分后的测试模块。

use super::*;

#[test]
fn fragments_roundtrip_every_type() {
    let samples = vec![
        ("string", RedisValue::Text("hello".into())),
        ("string", RedisValue::Bytes(vec![0xff, 0x00])),
        (
            "list",
            RedisValue::List(vec![
                RedisValue::Text("a".into()),
                RedisValue::Bytes(vec![1, 2]),
            ]),
        ),
        (
            "hash",
            RedisValue::Hash(vec![("f".into(), RedisValue::Text("v".into()))]),
        ),
        ("set", RedisValue::Set(vec![RedisValue::Text("m".into())])),
        (
            "zset",
            RedisValue::ZSet(vec![(RedisValue::Text("m".into()), 1.5)]),
        ),
        (
            "stream",
            RedisValue::Stream(vec![StreamEntry {
                id: "1-1".into(),
                fields: vec![("k".into(), "v".into())],
            }]),
        ),
    ];
    for (kind, value) in samples {
        let (fragment, _count) = encode_fragment(&value).unwrap();
        let decoded = decode_fragment(Some(kind), &fragment).unwrap();
        let (fragment2, _) = encode_fragment(&decoded).unwrap();
        assert_eq!(fragment, fragment2, "kind={kind}");
    }
}

#[test]
fn collection_fragment_requires_kind_but_string_is_self_described() {
    assert!(decode_fragment(None, &json!([{"t": "x"}])).is_err());
    assert!(matches!(
        decode_fragment(None, &json!({"text": "s"})).unwrap(),
        RedisValue::Text(_)
    ));
    assert!(matches!(
        decode_fragment(Some("set"), &json!([{"t": "m"}])).unwrap(),
        RedisValue::Set(_)
    ));
}

#[test]
fn item_counts_reported_for_summary() {
    let (_, count) = encode_fragment(&RedisValue::List(vec![
        RedisValue::Int(1),
        RedisValue::Int(2),
    ]))
    .unwrap();
    assert_eq!(count, 2);
    let (_, single) = encode_fragment(&RedisValue::Text("x".into())).unwrap();
    assert_eq!(single, 1);
}

#[test]
fn object_scope_only_accepts_declared_key_or_prefix() {
    let key = parse_export_scope(&json!({"scope": "key", "object": "users:1"}))
        .unwrap()
        .unwrap();
    assert!(key.contains("users:1"));
    assert!(!key.contains("users:2"));

    let prefix = parse_export_scope(&json!({"scope": "prefix", "object": "users"}))
        .unwrap()
        .unwrap();
    assert!(prefix.contains("users:1"));
    assert!(prefix.contains("users:nested:1"));
    assert!(!prefix.contains("users"));
    assert!(!prefix.contains("users2:1"));
    assert!(parse_export_scope(&json!({"scope": "other", "object": "x"})).is_err());
}
