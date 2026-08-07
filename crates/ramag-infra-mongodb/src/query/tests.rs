//! 拆分后的测试模块。

use super::*;
use bson::oid::ObjectId;
use serde_json::json;

#[test]
fn optional_query_document_rejects_non_object() {
    assert!(optional_document(Some(&json!([1, 2]))).is_err());
    assert!(optional_document(Some(&json!({"created_at": -1}))).is_ok());
    assert!(optional_document(None).unwrap().is_none());
}

#[test]
fn result_budget_only_reports_actual_overflow() {
    let mut budget = ResultBudget::default();
    assert!(budget.try_reserve(4, 8));
    assert!(budget.try_reserve(4, 8));
    assert_eq!(budget.documents, 2);
    assert_eq!(budget.retained_bytes, 8);

    assert!(!budget.try_reserve(1, 8));
    assert_eq!(budget.documents, 2);
    assert_eq!(budget.retained_bytes, 8);
}

#[test]
fn result_budget_rejects_byte_overflow_before_count_limit() {
    let mut budget = ResultBudget::default();
    assert!(budget.try_reserve(6, 8));
    assert!(!budget.try_reserve(3, 8));
    assert_eq!(budget.documents, 1);
    assert_eq!(budget.retained_bytes, 6);
}

#[test]
fn document_size_matches_bson_encoding() {
    let document = bson::doc! { "name": "ramag", "count": 3 };
    assert_eq!(
        document_size(&document).unwrap(),
        bson::to_vec(&document).unwrap().len()
    );
}

#[test]
fn find_limit_preserves_mongodb_zero_and_signed_semantics() {
    assert_eq!(effective_find_limit(None), 0);
    assert_eq!(effective_find_limit(Some(0)), 0);
    assert_eq!(effective_find_limit(Some(100)), 100);
    assert_eq!(effective_find_limit(Some(-100)), -100);
    assert_eq!(effective_find_limit(Some(100_000)), 100_000);
}

#[test]
fn command_bson_budget_accepts_boundary_and_rejects_overflow() {
    let document = bson::doc! { "a": "b" };
    let bytes = document_size(&document).unwrap();
    assert_eq!(
        reserve_command_document_bytes(0, &document, "test", bytes).unwrap(),
        bytes
    );
    assert!(reserve_command_document_bytes(1, &document, "test", bytes).is_err());
}

#[test]
fn format_objectid_extracts_hex() {
    let oid = ObjectId::new();
    let formatted = format_bson_id(&Bson::ObjectId(oid));
    assert_eq!(formatted.len(), 24);
    assert!(formatted.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn format_string_id_passthrough() {
    let v = Bson::String("custom-id".into());
    assert_eq!(format_bson_id(&v), "custom-id");
}

#[test]
fn known_command_name_is_promoted_to_first_bson_field() {
    let command = bson::doc! {
        "filter": {"name": "users"},
        "listCollections": 1,
        "nameOnly": false,
    };
    let ordered = promote_known_command(command);
    assert_eq!(
        ordered.keys().next().map(String::as_str),
        Some("listCollections")
    );
}

#[test]
fn only_id_index_duplicate_is_safe_to_skip() {
    assert!(duplicate_message_is_id_index(
        "E11000 duplicate key error collection: app.users index: _id_ dup key"
    ));
    assert!(duplicate_message_is_id_index(
        "E11000 duplicate key error index: app.users.$_id_ dup key"
    ));
    assert!(!duplicate_message_is_id_index(
        "E11000 duplicate key error collection: app.users index: email_1 dup key"
    ));
}
