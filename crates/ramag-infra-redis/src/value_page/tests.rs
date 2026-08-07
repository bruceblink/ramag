//! 拆分后的测试模块。

use super::*;

#[test]
fn page_items_bounds() {
    assert!(validate_page_items(1).is_ok());
    assert!(validate_page_items(MAX_PAGE_ITEMS).is_ok());
    assert!(validate_page_items(0).is_err());
    assert!(validate_page_items(MAX_PAGE_ITEMS + 1).is_err());
}

#[test]
fn cursor_kind_mismatch_rejected() {
    assert!(offset_cursor(&ValuePageCursor::Start, "List").is_ok());
    assert_eq!(
        offset_cursor(&ValuePageCursor::Offset(7), "List").ok(),
        Some(7)
    );
    assert!(offset_cursor(&ValuePageCursor::Scan(1), "List").is_err());
    assert!(scan_cursor(&ValuePageCursor::Offset(1), "Hash").is_err());
    assert_eq!(scan_cursor(&ValuePageCursor::Scan(9), "Hash").ok(), Some(9));
}

#[test]
fn member_arg_covers_scalar_forms() {
    assert_eq!(
        member_arg(&RedisValue::Text("a".into())).unwrap().as_ref(),
        b"a"
    );
    assert_eq!(
        member_arg(&RedisValue::Bytes(vec![0xff])).unwrap().as_ref(),
        &[0xff]
    );
    assert_eq!(member_arg(&RedisValue::Int(-3)).unwrap().as_ref(), b"-3");
    assert_eq!(
        member_arg(&RedisValue::Float(1.5)).unwrap().as_ref(),
        b"1.5"
    );
    assert!(member_arg(&RedisValue::Bool(true)).is_err());
}

#[test]
fn score_formatting_handles_edges() {
    assert_eq!(format_score(2.5).unwrap(), "2.5");
    assert_eq!(format_score(f64::INFINITY).unwrap(), "+inf");
    assert_eq!(format_score(f64::NEG_INFINITY).unwrap(), "-inf");
    assert!(format_score(f64::NAN).is_err());
}

#[test]
fn strict_hash_pairs_skips_binary_fields() {
    let flat = RV::Array(vec![
        RV::BulkString(b"ok".to_vec()),
        RV::BulkString(b"1".to_vec()),
        RV::BulkString(vec![0xff, 0xfe]),
        RV::BulkString(b"2".to_vec()),
    ]);
    let (pairs, skipped) = strict_hash_pairs(flat).unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0, "ok");
    assert_eq!(skipped, 1);
}

#[test]
fn strict_stream_entries_keep_raw_pagination_state() {
    let good = RV::Array(vec![
        RV::BulkString(b"1-1".to_vec()),
        RV::Array(vec![
            RV::BulkString(b"f".to_vec()),
            RV::BulkString(b"v".to_vec()),
        ]),
    ]);
    let bad = RV::Array(vec![
        RV::BulkString(b"1-2".to_vec()),
        RV::Array(vec![
            RV::BulkString(vec![0xff]),
            RV::BulkString(b"v".to_vec()),
        ]),
    ]);
    let (entries, raw_count, last_id, skipped) =
        strict_stream_entries(RV::Array(vec![good, bad])).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(raw_count, 2);
    assert_eq!(last_id.as_deref(), Some("1-2"));
    assert_eq!(skipped, 1);
}

#[test]
fn stream_entry_budget_rejects_oversized_id_or_total() {
    let oversized_id = StreamEntry {
        id: "x".repeat(MAX_REDIS_COMMAND_ARG_BYTES + 1),
        fields: vec![("f".into(), "v".into())],
    };
    assert!(validate_stream_entry_budget("key", &oversized_id).is_err());

    let oversized_total = StreamEntry {
        id: "1-0".into(),
        fields: vec![("f".into(), "v".repeat(WRITE_CHUNK_BYTES))],
    };
    assert!(validate_stream_entry_budget("key", &oversized_total).is_err());
}

#[test]
fn module_type_is_rejected_instead_of_being_treated_as_missing() {
    assert_eq!(
        supported_key_type("none", "missing").expect("none 表示不存在"),
        RedisType::None
    );
    let error =
        supported_key_type("ReJSON-RL", "document").expect_err("模块自定义类型必须明确拒绝");
    assert!(error.message().contains("ReJSON-RL"));
}
