use super::*;
use serde_json::json;

#[test]
fn flatten_scalar_object() {
    let t = build_flat_table(&[json!({"a": 1, "b": "x"})]);
    assert_eq!(t.columns.len(), 2);
    let a = t.columns.iter().find(|c| c.path == "a").unwrap();
    assert_eq!(a.kind, "int");
}

#[test]
fn sparse_column_matrix_is_bounded() {
    let docs: Vec<_> = (0..(MAX_TABLE_COLUMNS + 1))
        .map(|index| json!({(format!("field_{index}")): index}))
        .collect();

    let table = build_flat_table(&docs);

    assert_eq!(table.total_columns, MAX_TABLE_COLUMNS + 1);
    assert_eq!(table.columns.len(), MAX_TABLE_COLUMNS);
    assert_eq!(table.rows.len(), docs.len());
    assert_eq!(table_column_limit(50_000), 40);
}

#[test]
fn cancelled_table_work_stops_before_scanning_documents() {
    let docs = vec![json!({"name": "Alice"})];
    let cancelled = AtomicBool::new(true);

    assert!(build_flat_table_with_cancellable(&docs, &BTreeSet::new(), &cancelled).is_none());
    assert!(collect_paths_cancellable(&docs, 4, &cancelled).is_none());
}

#[test]
fn wide_document_is_bounded_before_building_the_row_matrix() {
    let mut document = serde_json::Map::new();
    for index in 0..(MAX_TABLE_COLUMNS + 100) {
        document.insert(format!("field_{index}"), json!(index));
    }
    document.insert("_id".to_string(), json!("primary"));

    let table = build_flat_table(&[Value::Object(document)]);

    assert_eq!(table.columns.len(), MAX_TABLE_COLUMNS);
    assert_eq!(table.rows[0].len(), MAX_TABLE_COLUMNS);
    assert_eq!(table.columns[0].path, "_id");
    assert!(table.total_columns > table.columns.len());
}

#[test]
fn flatten_nested_object_is_summary() {
    let t = build_flat_table(&[json!({"specs": {"cpu": "i7", "ram": 16}})]);
    assert_eq!(t.columns.len(), 1);
    assert_eq!(t.columns[0].path, "specs");
    assert_eq!(t.columns[0].kind, "object");
    assert_eq!(t.rows[0][0].text, "{2 字段}");
}

#[test]
fn flatten_oid_unwrapped() {
    let t = build_flat_table(&[json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})]);
    let cell = &t.rows[0][0];
    assert_eq!(cell.kind, "oid");
    assert_eq!(cell.text, "507f1f77bcf86cd799439011");
}

#[test]
fn flatten_decimal_unwrapped() {
    let t = build_flat_table(&[json!({"price": {"$numberDecimal": "1299.99"}})]);
    assert_eq!(t.rows[0][0].kind, "decimal");
    assert_eq!(t.rows[0][0].text, "1299.99");
}

#[test]
fn flatten_array_is_summary() {
    let t = build_flat_table(&[json!({"tags": ["a", "b"]})]);
    assert_eq!(t.rows[0][0].kind, "array");
    assert_eq!(t.rows[0][0].text, "[2 项]");
}

#[test]
fn flatten_columns_id_first() {
    let t = build_flat_table(&[json!({"name": "a", "_id": "x"})]);
    assert_eq!(t.columns[0].path, "_id");
}

#[test]
fn flatten_missing_field_filled_null() {
    let t = build_flat_table(&[json!({"a": 1}), json!({"b": 2})]);
    assert_eq!(t.columns.len(), 2);
    assert_eq!(t.rows[0].len(), 2);
    assert_eq!(t.rows[1].len(), 2);
}

#[test]
fn flatten_date_canonical_form() {
    let t = build_flat_table(&[json!({"ts": {"$date": {"$numberLong": "1700000000000"}}})]);
    assert_eq!(t.rows[0][0].kind, "date");
    assert_eq!(t.rows[0][0].text, "1700000000000");
}

#[test]
fn flatten_timestamp() {
    let t = build_flat_table(&[json!({"ts": {"$timestamp": {"t": 1700, "i": 5}}})]);
    assert_eq!(t.rows[0][0].kind, "ts");
    assert!(t.rows[0][0].text.contains("1700"));
}

#[test]
fn flatten_regex() {
    let t = build_flat_table(&[json!({
        "rx": {"$regularExpression": {"pattern": "^abc", "options": "i"}}
    })]);
    assert_eq!(t.rows[0][0].kind, "regex");
    assert_eq!(t.rows[0][0].text, "/^abc/i");
}

#[test]
fn flatten_minkey_maxkey() {
    let t = build_flat_table(&[json!({"lo": {"$minKey": 1}, "hi": {"$maxKey": 1}})]);
    let lo = t
        .columns
        .iter()
        .position(|c| c.path == "lo")
        .map(|i| &t.rows[0][i])
        .unwrap();
    let hi = t
        .columns
        .iter()
        .position(|c| c.path == "hi")
        .map(|i| &t.rows[0][i])
        .unwrap();
    assert_eq!(lo.kind, "minkey");
    assert_eq!(hi.kind, "maxkey");
}

#[test]
fn flatten_undefined() {
    let t = build_flat_table(&[json!({"x": {"$undefined": true}})]);
    assert_eq!(t.rows[0][0].kind, "undef");
    assert_eq!(t.rows[0][0].text, "undefined");
}

#[test]
fn flatten_code_and_symbol() {
    let t = build_flat_table(&[json!({
        "fn": {"$code": "function(){}"},
        "sym": {"$symbol": "alpha"}
    })]);
    let f = t
        .columns
        .iter()
        .position(|c| c.path == "fn")
        .map(|i| &t.rows[0][i])
        .unwrap();
    let s = t
        .columns
        .iter()
        .position(|c| c.path == "sym")
        .map(|i| &t.rows[0][i])
        .unwrap();
    assert_eq!(f.kind, "code");
    assert_eq!(s.kind, "symbol");
}

#[test]
fn flatten_int32_canonical() {
    let t = build_flat_table(&[json!({"n": {"$numberInt": "42"}})]);
    assert_eq!(t.rows[0][0].kind, "int");
    assert_eq!(t.rows[0][0].text, "42");
}

#[test]
fn flatten_int64_numberlong_is_long() {
    let t = build_flat_table(&[json!({"n": {"$numberLong": "9999999999"}})]);
    assert_eq!(t.rows[0][0].kind, "long");
    assert_eq!(t.rows[0][0].text, "9999999999");
}

#[test]
fn flatten_double_canonical() {
    let t = build_flat_table(&[json!({"d": {"$numberDouble": "Infinity"}})]);
    assert_eq!(t.rows[0][0].kind, "double");
    assert_eq!(t.rows[0][0].text, "Infinity");
}

#[test]
fn flatten_binary_with_subtype() {
    let t = build_flat_table(&[json!({
        "blob": {"$binary": {"base64": "aGVsbG8=", "subType": "00"}}
    })]);
    assert_eq!(t.rows[0][0].kind, "binary");
    assert!(t.rows[0][0].text.contains("subType=00"));
}

#[test]
fn expand_object_path_into_subcolumns() {
    let docs = vec![json!({"consume": {"cost": 12, "name": "x"}, "id": 1})];
    let exp = BTreeSet::from(["consume".to_string()]);
    let t = build_flat_table_with(&docs, &exp);
    assert!(t.columns.iter().any(|c| c.path == "consume.cost"));
    assert!(t.columns.iter().any(|c| c.path == "consume.name"));
    assert!(!t.columns.iter().any(|c| c.path == "consume"));
}

#[test]
fn expand_nested_two_levels() {
    let docs = vec![json!({"a": {"b": {"c": 1}}})];
    let exp = BTreeSet::from(["a".to_string(), "a.b".to_string()]);
    let t = build_flat_table_with(&docs, &exp);
    assert!(t.columns.iter().any(|c| c.path == "a.b.c"));
}

#[test]
fn expand_skips_extjson_wrapper() {
    let docs = vec![json!({"_id": {"$oid": "507f1f77bcf86cd799439011"}})];
    let exp = BTreeSet::from(["_id".to_string()]);
    let t = build_flat_table_with(&docs, &exp);
    assert_eq!(t.rows[0][0].kind, "oid");
    assert_eq!(t.rows[0][0].text, "507f1f77bcf86cd799439011");
}

#[test]
fn no_expand_keeps_summary() {
    let t = build_flat_table(&[json!({"consume": {"cost": 12}})]);
    assert_eq!(t.rows[0][0].kind, "object");
    assert_eq!(t.rows[0][0].text, "{1 字段}");
}

#[test]
fn collect_paths_includes_nested() {
    let docs = vec![json!({"consume": {"cost": 1, "detail": {"x": 2}}, "id": 1})];
    let paths = collect_paths(&docs, 4);
    for want in [
        "consume",
        "consume.cost",
        "consume.detail",
        "consume.detail.x",
        "id",
    ] {
        assert!(paths.contains(&want.to_string()), "missing {want}");
    }
}

#[test]
fn collect_paths_skips_extjson() {
    let docs = vec![json!({"_id": {"$oid": "abc"}})];
    let paths = collect_paths(&docs, 4);
    assert!(paths.contains(&"_id".to_string()));
    assert!(!paths.iter().any(|p| p.contains("$oid")));
}

#[test]
fn collect_paths_through_array() {
    let docs = vec![json!({"jobs": [{"connectors": {"x": 1}, "cover": 2}]})];
    let paths = collect_paths(&docs, 5);
    for want in ["jobs", "jobs.connectors", "jobs.cover", "jobs.connectors.x"] {
        assert!(paths.contains(&want.to_string()), "missing {want}");
    }
}

#[test]
fn prepend_lead_inserts_leading_columns() {
    let mut t = build_flat_table(&[json!({"a": 1}), json!({"a": 2})]);
    let lead = vec![Column {
        path: "‹父1›".to_string(),
        kind: "text",
    }];
    let lead_rows = vec![
        vec![Cell {
            text: "p1".to_string(),
            kind: "text",
        }],
        vec![Cell {
            text: "p2".to_string(),
            kind: "text",
        }],
    ];
    t.prepend_lead(lead, lead_rows);
    assert_eq!(t.columns[0].path, "‹父1›");
    assert!(t.columns.iter().any(|c| c.path == "a"));
    assert_eq!(t.rows[0][0].text, "p1");
    assert_eq!(t.rows[1][0].text, "p2");
    assert_eq!(t.rows[0].len(), t.columns.len());
}

#[test]
fn constant_lead_shares_column_and_cell_budget() {
    let empty = Cell {
        text: String::new(),
        kind: "null",
    };
    let mut table = FlatTable {
        columns: (0..MAX_TABLE_COLUMNS)
            .map(|index| Column {
                path: format!("c{index}"),
                kind: "text",
            })
            .collect(),
        total_columns: MAX_TABLE_COLUMNS,
        rows: vec![vec![empty; MAX_TABLE_COLUMNS]],
    };

    table.prepend_constant_lead(vec![(
        Column {
            path: "parent".into(),
            kind: "text",
        },
        Cell {
            text: "id".into(),
            kind: "text",
        },
    )]);

    assert_eq!(table.columns.len(), MAX_TABLE_COLUMNS);
    assert_eq!(table.rows[0].len(), MAX_TABLE_COLUMNS);
    assert_eq!(table.total_columns, MAX_TABLE_COLUMNS + 1);
}
