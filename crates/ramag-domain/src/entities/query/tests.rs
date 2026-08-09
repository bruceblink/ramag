use super::*;
use chrono::TimeZone;

#[test]
fn clipboard_null_is_empty() {
    assert_eq!(Value::Null.to_clipboard_string(), "");
}

#[test]
fn clipboard_primitive() {
    assert_eq!(Value::Bool(true).to_clipboard_string(), "true");
    assert_eq!(Value::Int(-42).to_clipboard_string(), "-42");
    assert_eq!(Value::Float(2.5).to_clipboard_string(), "2.5");
}

#[test]
fn query_match_is_case_insensitive_for_ascii_and_unicode() {
    assert!(Value::Text("Hello Rust".into()).contains_query_lower("hello"));
    assert!(Value::Text("你好世界".into()).contains_query_lower("世界"));
    assert!(!Value::Text("Hello".into()).contains_query_lower("world"));
    let json = Value::Json(serde_json::json!({"UserName": ["Alice", "你好世界"]}));
    assert!(json.contains_query_lower("username"));
    assert!(json.contains_query_lower("alice"));
    assert!(json.contains_query_lower("世界"));
}

#[test]
fn preview_truncates_without_splitting_unicode() {
    assert_eq!(Value::Text("你好世界".into()).display_preview(2), "你好…");
    assert_eq!(Value::Text("你好".into()).display_preview(2), "你好");
}

#[test]
fn pretty_json_limit_is_enforced_during_serialization() -> std::result::Result<(), serde_json::Error>
{
    let value = serde_json::json!({"name": "alice", "items": [1, 2]});
    let expected = serde_json::to_string_pretty(&value)?;

    assert_eq!(
        json_pretty_bounded(&value, expected.len()),
        Some(expected.clone())
    );
    assert!(json_pretty_bounded(&value, expected.len() - 1).is_none());
    Ok(())
}

#[test]
fn clipboard_text_not_truncated() {
    let long: String = "字".repeat(200);
    assert_eq!(Value::Text(long.clone()).to_clipboard_string(), long);
}

#[test]
fn clipboard_bytes_hex() {
    let v = Value::Bytes(vec![0x00, 0xAB, 0xff]);
    assert_eq!(v.to_clipboard_string(), "00abff");
}

#[test]
fn clipboard_datetime_rfc3339() {
    let dt = chrono::Utc
        .with_ymd_and_hms(2026, 4, 26, 17, 30, 0)
        .unwrap();
    let s = Value::DateTime(dt).to_clipboard_string();
    assert!(s.starts_with("2026-04-26T17:30:00"));
}

#[test]
fn sql_literal_basic() {
    assert_eq!(Value::Null.to_sql_literal(), "NULL");
    assert_eq!(Value::Bool(true).to_sql_literal(), "TRUE");
    assert_eq!(Value::Bool(false).to_sql_literal(), "FALSE");
    assert_eq!(Value::Int(42).to_sql_literal(), "42");
}

#[test]
fn sql_literal_text_escapes_quote() {
    assert_eq!(
        Value::Text("O'Reilly".to_string()).to_sql_literal(),
        "'O''Reilly'"
    );
    assert_eq!(Value::Text("a\\b".to_string()).to_sql_literal(), "'a\\\\b'");
}

#[test]
fn sql_literal_bytes_hex() {
    assert_eq!(
        Value::Bytes(vec![0x00, 0xab, 0xff]).to_sql_literal(),
        "0x00abff"
    );
}

#[test]
fn sql_literal_datetime_mysql_format() {
    let dt = chrono::Utc
        .with_ymd_and_hms(2026, 4, 8, 17, 31, 15)
        .unwrap();
    assert_eq!(
        Value::DateTime(dt).to_sql_literal(),
        "'2026-04-08 17:31:15.000000'"
    );
}

#[test]
fn sql_literal_pg_dialect() {
    use super::super::connection::DriverKind;
    assert_eq!(
        Value::Text("a\\b".to_string()).to_sql_literal_for(DriverKind::Postgres),
        "'a\\b'"
    );
    assert_eq!(
        Value::Bytes(vec![0xde, 0xad]).to_sql_literal_for(DriverKind::Postgres),
        "'\\xdead'"
    );
    assert_eq!(
        Value::Text("O'x".to_string()).to_sql_literal_for(DriverKind::Postgres),
        "'O''x'"
    );
}

#[test]
fn sql_literal_length_is_checked_without_building_large_output() {
    use super::super::connection::DriverKind;

    let text = Value::Text("a'\\b".into());
    let mysql = text.to_sql_literal_for(DriverKind::Mysql);
    assert_eq!(
        text.bounded_sql_literal_len_for(DriverKind::Mysql, mysql.len()),
        Some(mysql.len())
    );
    assert!(
        text.bounded_sql_literal_len_for(DriverKind::Mysql, mysql.len() - 1)
            .is_none()
    );

    let json = Value::Json(serde_json::json!({"text": "O'Reilly\\path"}));
    let postgres = json.to_sql_literal_for(DriverKind::Postgres);
    assert_eq!(
        json.bounded_sql_literal_len_for(DriverKind::Postgres, postgres.len()),
        Some(postgres.len())
    );
}

#[test]
fn preview_text_strips_newlines() {
    let v = Value::Text("line1\nline2\r\nline3".to_string());
    let p = v.display_preview(80);
    assert!(!p.contains('\n') && !p.contains('\r'));
}

#[test]
fn clipboard_json_minified() {
    let v = Value::Json(serde_json::json!({"a": 1, "b": [2, 3]}));
    let s = v.to_clipboard_string();
    assert!(!s.contains("\n"));
    assert!(s.contains("\"a\":1"));
}

#[test]
fn row_retained_bytes_tracks_dynamic_payloads() {
    let small = Row {
        values: vec![Value::Text("a".into())],
    };
    let large = Row {
        values: vec![
            Value::Text("x".repeat(1024)),
            Value::Bytes(vec![0; 2048]),
            Value::Json(serde_json::json!({"items": ["y".repeat(512)]})),
        ],
    };

    assert!(large.retained_bytes() > small.retained_bytes() + 3_000);
}

#[test]
fn sql_query_and_schema_have_explicit_boundaries() {
    let valid = Query::new("x".repeat(MAX_SQL_QUERY_BYTES));
    assert!(valid.validate().is_ok());
    assert!(
        Query::new("x".repeat(MAX_SQL_QUERY_BYTES + 1))
            .validate()
            .is_err()
    );
    assert!(Query::new("select\0 1").validate().is_err());
    assert!(
        Query::new("select 1")
            .with_result_byte_limit(super::super::TRANSFER_BATCH_BYTES)
            .validate()
            .is_ok()
    );
    assert!(
        Query::new("select 1")
            .with_result_byte_limit(0)
            .validate()
            .is_err()
    );
    assert!(
        Query::new("select 1")
            .with_result_byte_limit(super::super::MAX_INTERACTIVE_RESULT_BYTES + 1)
            .validate()
            .is_err()
    );

    let mut bad_schema = Query::new("select 1");
    bad_schema.default_schema = Some("bad\nschema".into());
    assert!(bad_schema.validate().is_err());
}

#[test]
fn edit_display_is_bounded_before_large_hex_or_json_allocations() {
    for value in [
        Value::Text("你".repeat(100)),
        Value::Bytes(vec![0xab; 100]),
        Value::Json(serde_json::json!({"items": vec!["value"; 100]})),
    ] {
        let (display, truncated) = value.display_for_edit_bounded(64);
        assert!(truncated);
        assert!(display.len() <= 64);
    }

    let (small, truncated) = Value::Json(serde_json::json!({"a": 1})).display_for_edit_bounded(128);
    assert!(!truncated);
    assert!(small.contains('\n'));
}
