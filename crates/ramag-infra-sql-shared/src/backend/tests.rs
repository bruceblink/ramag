use super::{
    MAX_QUERY_RESULT_BYTES, MAX_QUERY_WARNINGS, QUERY_RESULT_MEMORY_WARNING_BYTES,
    QueryResultLimit, append_warnings_bounded, query_result_memory_warning, try_push_query_row,
    validate_backend_config, validate_metadata_identifier, validate_query_columns_with_limits,
};
use ramag_domain::entities::{ConnectionConfig, DriverKind, Row, Value, Warning};

fn warnings(count: usize) -> Vec<Warning> {
    (0..count)
        .map(|index| Warning {
            level: "Warning".into(),
            code: index as u32,
            message: format!("warning {index}"),
        })
        .collect()
}

#[test]
fn warning_budget_keeps_exact_boundary() {
    let mut accumulated = Vec::new();
    append_warnings_bounded(&mut accumulated, warnings(MAX_QUERY_WARNINGS));

    assert_eq!(accumulated.len(), MAX_QUERY_WARNINGS);
    assert_ne!(
        accumulated.last().map(|warning| warning.level.as_str()),
        Some("Client")
    );
}

#[test]
fn warning_budget_replaces_tail_with_truncation_marker() {
    let mut accumulated = warnings(MAX_QUERY_WARNINGS);
    append_warnings_bounded(&mut accumulated, warnings(1));

    assert_eq!(accumulated.len(), MAX_QUERY_WARNINGS);
    assert_eq!(
        accumulated.last().map(|warning| warning.level.as_str()),
        Some("Client")
    );
    assert_eq!(accumulated.last().map(|warning| warning.code), Some(0));
    assert!(
        accumulated
            .last()
            .is_some_and(|warning| warning.message.contains("仅保留前"))
    );
}

#[test]
fn query_row_budget_enforces_bytes() {
    let row = || Row {
        values: vec![Value::Text("x".repeat(128))],
    };
    let one_row_bytes = row().retained_bytes();
    let mut rows = Vec::new();
    let mut retained_bytes = 0;

    assert_eq!(
        try_push_query_row(&mut rows, &mut retained_bytes, row(), one_row_bytes),
        Ok(())
    );
    assert_eq!(
        try_push_query_row(&mut rows, &mut retained_bytes, row(), one_row_bytes),
        Err(QueryResultLimit::Bytes)
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(retained_bytes, one_row_bytes);
}

#[test]
fn query_result_memory_has_distinct_warning_and_truncation_thresholds() {
    assert!(
        query_result_memory_warning(
            QUERY_RESULT_MEMORY_WARNING_BYTES - 1,
            false,
            10,
            MAX_QUERY_RESULT_BYTES,
        )
        .is_none()
    );

    let warning = query_result_memory_warning(
        QUERY_RESULT_MEMORY_WARNING_BYTES,
        false,
        20,
        MAX_QUERY_RESULT_BYTES,
    );
    assert!(
        warning
            .as_ref()
            .is_some_and(|warning| warning.message.contains("128 MiB"))
    );
    assert!(
        warning
            .as_ref()
            .is_some_and(|warning| warning.message.contains("未截断"))
    );
    assert!(
        warning
            .as_ref()
            .is_some_and(|warning| warning.message.contains("256 MiB"))
    );

    let truncated =
        query_result_memory_warning(MAX_QUERY_RESULT_BYTES, true, 30, MAX_QUERY_RESULT_BYTES);
    assert!(
        truncated
            .as_ref()
            .is_some_and(|warning| warning.message.contains("256 MiB"))
    );
    assert!(
        truncated
            .as_ref()
            .is_some_and(|warning| warning.message.contains("已截断"))
    );
    assert!(
        truncated
            .as_ref()
            .is_some_and(|warning| !warning.message.contains("未截断"))
    );

    let transfer_truncated = query_result_memory_warning(
        31 * 1024 * 1024,
        true,
        12,
        ramag_domain::entities::TRANSFER_BATCH_BYTES as u64,
    );
    assert!(
        transfer_truncated
            .as_ref()
            .is_some_and(|warning| warning.message.contains("32 MiB"))
    );
}

#[test]
fn query_result_column_metadata_is_bounded_and_consistent() {
    let columns = vec!["a".to_string(), "bb".to_string()];
    let types = vec!["x".to_string(), "yy".to_string()];

    assert!(matches!(
        validate_query_columns_with_limits(&columns, &types, 2, 6),
        Ok(6)
    ));
    assert!(validate_query_columns_with_limits(&columns, &types, 1, 6).is_err());
    assert!(validate_query_columns_with_limits(&columns, &types, 2, 5).is_err());
    assert!(validate_query_columns_with_limits(&columns, &types[..1], 2, 6).is_err());
}

#[test]
fn backend_validation_runs_before_pool_cache_lookup() {
    let config = ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root");
    assert!(validate_backend_config(&config, DriverKind::Mysql, "mysql").is_ok());
    assert!(validate_backend_config(&config, DriverKind::Postgres, "postgres").is_err());

    let mut invalid = config;
    invalid.port = 0;
    assert!(validate_backend_config(&invalid, DriverKind::Mysql, "mysql").is_err());
}

#[test]
fn metadata_identifiers_are_validated_before_pool_lookup() {
    assert!(validate_metadata_identifier("public", "schema").is_ok());
    assert!(validate_metadata_identifier("", "schema").is_err());
    assert!(validate_metadata_identifier("bad\nname", "table").is_err());
    assert!(
        validate_metadata_identifier(
            &"x".repeat(ramag_domain::entities::MAX_CONNECTION_IDENTIFIER_BYTES + 1),
            "table",
        )
        .is_err()
    );
}
