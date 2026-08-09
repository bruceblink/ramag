use super::*;
use ramag_domain::entities::Row;

fn sample_result() -> QueryResult {
    QueryResult {
        columns: vec!["id".into(), "name".into(), "data".into()],
        column_types: Vec::new(),
        rows: vec![
            Row {
                values: vec![Value::Int(1), Value::Text("张三".into()), Value::Null],
            },
            Row {
                values: vec![
                    Value::Int(2),
                    Value::Text("李, 四".into()),
                    Value::Text("\"escaped\"".into()),
                ],
            },
        ],
        affected_rows: 0,
        elapsed_ms: 5,
        warnings: Vec::new(),
        truncated: false,
    }
}

#[test]
fn export_file_name_contains_type_database_and_object() {
    assert_eq!(
        suggested_export_file_name_at(
            "MySQL",
            "ramag/demo",
            Some("order:items"),
            false,
            ".SQL",
            "20260722-123456",
        ),
        "ramag-mysql-ramag_demo-order_items-20260722-123456.sql"
    );
}

#[test]
fn export_file_name_omits_object_and_marks_data_scope() {
    assert_eq!(
        suggested_export_file_name_at(
            "MongoDB",
            "ramag_demo",
            None,
            true,
            "jsonl",
            "20260722-123456",
        ),
        "ramag-mongodb-ramag_demo-data-20260722-123456.jsonl"
    );
}

#[test]
fn export_file_name_is_bounded_and_utf8_safe() {
    let long_name = "数据库".repeat(100);
    let name = suggested_export_file_name_at(
        "PostgreSQL",
        &long_name,
        Some(&long_name),
        false,
        "jsonl",
        "20260722-123456",
    );

    assert!(name.len() <= 255);
    assert!(name.starts_with("ramag-postgresql-"));
    assert!(name.ends_with("-20260722-123456.jsonl"));
    assert!(!name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']));
}

#[test]
fn jsonl_basic() {
    let mut output = Vec::new();
    write_jsonl(&mut output, &sample_result()).unwrap();
    let text = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["id"], 1);
    assert_eq!(first["name"], "张三");
    assert!(first["data"].is_null());
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["name"], "李, 四");
}

#[test]
fn indexed_view_preserves_row_order_and_column_projection() {
    let result = sample_result();
    let rows = [1, 0];
    let columns = [1, 0];
    let mut output = Vec::new();

    write_jsonl_view(&mut output, &result, Some(&rows), Some(&columns)).unwrap();

    let text = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let row0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row0["id"], 2);
    assert_eq!(row0["name"], "李, 四");
    assert!(row0.get("data").is_none());
    let row1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(row1["id"], 1);
}

#[test]
fn indexed_view_writes_null_for_missing_selected_value() {
    let mut result = sample_result();
    result.rows[0].values.truncate(1);
    let rows = [0];
    let columns = [0, 2];
    let mut output = Vec::new();

    write_jsonl_view(&mut output, &result, Some(&rows), Some(&columns)).unwrap();

    let text = String::from_utf8(output).unwrap();
    let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert!(line["data"].is_null());
}

#[test]
fn json_projection_sorts_once_and_keeps_last_duplicate_column() {
    let result = QueryResult {
        columns: vec!["z".into(), "same".into(), "a".into(), "same".into()],
        column_types: Vec::new(),
        rows: vec![Row {
            values: vec![
                Value::Int(1),
                Value::Text("old".into()),
                Value::Int(2),
                Value::Text("new".into()),
            ],
        }],
        affected_rows: 0,
        elapsed_ms: 0,
        warnings: Vec::new(),
        truncated: false,
    };
    let projection = json_projection(&result, None);
    assert_eq!(projection, vec![("a", 2), ("same", 3), ("z", 0)]);

    let mut output = Vec::new();
    write_jsonl(&mut output, &result).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["same"], "new");
}

#[test]
fn atomic_write_replaces_complete_file_without_temp_remnant() -> std::io::Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "ramag-export-test-{}-{}",
        std::process::id(),
        EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir)?;
    let path = dir.join("result.csv");
    std::fs::write(&path, "old")?;

    write_atomic(&path, "new content").map_err(std::io::Error::other)?;

    assert_eq!(std::fs::read_to_string(&path)?, "new content");
    assert_eq!(std::fs::read_dir(&dir)?.count(), 1);
    std::fs::remove_dir_all(dir)
}

#[test]
fn failed_streaming_export_preserves_original_and_removes_temp() -> std::io::Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "ramag-export-failure-test-{}-{}",
        std::process::id(),
        EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir)?;
    let path = dir.join("result.csv");
    std::fs::write(&path, "old")?;

    let result = write_atomic_with(&path, |writer| {
        writer
            .write_all(b"partial")
            .map_err(|error| DomainError::Storage(error.to_string()))?;
        Err(DomainError::Storage("test export failure".into()))
    });

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path)?, "old");
    assert_eq!(std::fs::read_dir(&dir)?.count(), 1);
    std::fs::remove_dir_all(dir)
}

#[cfg(unix)]
#[test]
fn new_atomic_export_is_private() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!(
        "ramag-export-mode-test-{}-{}",
        std::process::id(),
        EXPORT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir)?;
    let path = dir.join("result.json");

    write_atomic(&path, "[]").map_err(std::io::Error::other)?;

    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_dir_all(dir)
}
