use super::*;

#[test]
fn parse_version_finds_field() {
    let info = "# Server\r\nredis_version:7.2.4\r\nredis_mode:standalone\r\n";
    assert_eq!(parse_redis_version(info).unwrap(), "7.2.4");
}

#[test]
fn parse_version_missing_returns_error() {
    assert!(parse_redis_version("# Server\r\nfoo:bar\r\n").is_err());
}

#[test]
fn ttl_rejects_values_that_redis_would_treat_as_delete() {
    assert!(validate_ttl_secs(None).is_ok());
    assert!(validate_ttl_secs(Some(1)).is_ok());
    assert!(validate_ttl_secs(Some(0)).is_err());
    assert!(validate_ttl_secs(Some(-1)).is_err());
}

#[test]
fn parse_scan_basic() {
    let v = RV::Array(vec![
        RV::BulkString(b"123".to_vec()),
        RV::Array(vec![
            RV::BulkString(b"key1".to_vec()),
            RV::BulkString(b"key2".to_vec()),
        ]),
    ]);
    let r = parse_scan_response(v).unwrap();
    assert_eq!(r.cursor, 123);
    assert_eq!(r.keys.len(), 2);
    assert_eq!(r.keys[0].key, "key1");
    assert_eq!(r.keys[1].key, "key2");
}

#[test]
fn parse_scan_end_cursor_zero() {
    let v = RV::Array(vec![RV::BulkString(b"0".to_vec()), RV::Array(vec![])]);
    let r = parse_scan_response(v).unwrap();
    assert_eq!(r.cursor, 0);
    assert!(r.keys.is_empty());
}

#[test]
fn scan_parts_preserves_cursor_and_payload() {
    let response = RV::Array(vec![
        RV::BulkString(b"9".to_vec()),
        RV::Array(vec![RV::BulkString(b"member".to_vec())]),
    ]);

    let (cursor, payload) = scan_parts(response, "SSCAN").unwrap();

    assert_eq!(cursor, 9);
    assert!(matches!(payload, RV::Array(values) if values.len() == 1));
}

#[test]
fn scan_cursor_rejects_negative_integer() {
    assert!(parse_cursor(RV::Int(-1), "HSCAN").is_err());
    assert!(parse_scan_response(RV::Array(vec![RV::Int(-1), RV::Array(vec![])])).is_err());
}

#[test]
fn scan_rejects_keys_that_cannot_be_addressed_safely() {
    let binary_key = RV::Array(vec![
        RV::BulkString(b"0".to_vec()),
        RV::Array(vec![RV::BulkString(vec![0xff])]),
    ]);
    assert!(parse_scan_response(binary_key).is_err());

    let invalid_type = RV::Array(vec![
        RV::BulkString(b"0".to_vec()),
        RV::Array(vec![RV::Int(42)]),
    ]);
    assert!(parse_scan_response(invalid_type).is_err());
}

#[test]
fn truncated_string_drops_only_incomplete_utf8_tail() {
    let value = decode_string_prefix(RV::BulkString(vec![b'a', 0xe4, 0xb8]), true);
    assert!(matches!(value, RedisValue::Text(text) if text == "a"));

    let binary = decode_string_prefix(RV::BulkString(vec![0xff, 0xfe]), true);
    assert!(matches!(binary, RedisValue::Bytes(bytes) if bytes == vec![0xff, 0xfe]));
}

#[test]
fn response_budget_rejects_bytes_nodes_and_depth() {
    assert_eq!(MAX_RESPONSE_BYTES, MAX_REDIS_COLLECTION_BYTES);
    let limits = ResponseLimits {
        bytes: 3,
        nodes: 3,
        depth: 2,
    };
    assert!(ensure_response_with_limits(&RV::BulkString(b"abc".to_vec()), "test", limits).is_ok());
    assert!(
        ensure_response_with_limits(&RV::BulkString(b"abcd".to_vec()), "test", limits).is_err()
    );
    assert!(
        ensure_response_with_limits(
            &RV::Array(vec![RV::Int(1), RV::Int(2), RV::Int(3)]),
            "test",
            limits
        )
        .is_err()
    );
    assert!(
        ensure_response_with_limits(
            &RV::Array(vec![RV::Array(vec![RV::Array(vec![RV::Nil])])]),
            "test",
            limits
        )
        .is_err()
    );
}

#[test]
fn info_sections_have_explicit_argument_boundaries() {
    assert!(validate_info_sections(&[]).is_ok());
    assert!(validate_info_sections(&["server", "memory"]).is_ok());
    assert!(validate_info_sections(&[""]).is_err());
    assert!(validate_info_sections(&["bad section"]).is_err());
    let oversized = "x".repeat(257);
    assert!(validate_info_sections(&[oversized.as_str()]).is_err());
    let excessive = ["server"; 33];
    assert!(validate_info_sections(&excessive).is_err());
}

#[test]
fn retained_collection_budget_accepts_boundary_and_rejects_overflow() {
    assert_eq!(reserve_retained_bytes(3, 2, 5), Some(5));
    assert_eq!(reserve_retained_bytes(3, 3, 5), None);
    assert_eq!(reserve_retained_bytes(usize::MAX, 1, usize::MAX), None);

    let short = redis_value_retained_bytes(&RedisValue::Text("a".into()));
    let long = redis_value_retained_bytes(&RedisValue::Text("abcd".into()));
    assert_eq!(long - short, 3);
}
