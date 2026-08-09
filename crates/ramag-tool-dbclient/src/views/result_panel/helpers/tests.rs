use super::*;
use ramag_domain::entities::{ColumnKind, DriverKind, QueryResult, Row, Value};

/// 通过 `to_sql_literal` 把 Value 拍成可比较字符串（Value 没实现 PartialEq）
fn lit(v: &Value) -> String {
    v.to_sql_literal()
}

fn make_result(cols: &[&str]) -> QueryResult {
    QueryResult {
        columns: cols.iter().map(|s| s.to_string()).collect(),
        column_types: vec![String::new(); cols.len()],
        rows: vec![],
        warnings: vec![],
        elapsed_ms: 0,
        affected_rows: 0,
        truncated: false,
    }
}

#[test]
fn batch_delete_sql_budget_has_an_exact_boundary() {
    assert_eq!(
        reserve_batch_delete_sql_bytes(MAX_BATCH_DELETE_SQL_BYTES - 1, 1),
        Some(MAX_BATCH_DELETE_SQL_BYTES)
    );
    assert_eq!(
        reserve_batch_delete_sql_bytes(MAX_BATCH_DELETE_SQL_BYTES, 1),
        None
    );
}

#[test]
fn parse_value_empty_nullable() {
    let v = parse_value_for_kind(ColumnKind::Text, "", true, false).unwrap();
    assert_eq!(lit(v.as_ref().unwrap()), "NULL");
}

#[test]
fn parse_value_empty_with_default() {
    let v = parse_value_for_kind(ColumnKind::Text, "  ", false, true).unwrap();
    assert!(v.is_none(), "留空 + 有 default → 跳过让 DB 用 DEFAULT");
}

#[test]
fn parse_value_empty_required() {
    let err = parse_value_for_kind(ColumnKind::Text, "", false, false).unwrap_err();
    assert!(err.contains("必填"));
}

#[test]
fn parse_value_explicit_null_nullable() {
    for s in ["NULL", "null", "Null"] {
        let v = parse_value_for_kind(ColumnKind::Integer, s, true, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "NULL", "input={s}");
    }
}

#[test]
fn parse_value_explicit_null_not_nullable() {
    let err = parse_value_for_kind(ColumnKind::Integer, "NULL", false, true).unwrap_err();
    assert!(err.contains("不可为 NULL"));
}

#[test]
fn parse_value_integer_ok() {
    let v = parse_value_for_kind(ColumnKind::Integer, "42", false, false).unwrap();
    assert_eq!(lit(v.as_ref().unwrap()), "42");
}

#[test]
fn parse_value_integer_negative() {
    let v = parse_value_for_kind(ColumnKind::Integer, "-7", false, false).unwrap();
    assert_eq!(lit(v.as_ref().unwrap()), "-7");
}

#[test]
fn parse_value_integer_invalid() {
    let err = parse_value_for_kind(ColumnKind::Integer, "abc", false, false).unwrap_err();
    assert!(err.contains("不是合法整数"));
}

#[test]
fn parse_value_float_ok() {
    let v = parse_value_for_kind(ColumnKind::Float, "3.5", false, false).unwrap();
    assert!(matches!(v, Some(Value::Float(_))));
    assert_eq!(lit(v.as_ref().unwrap()), "3.5");
}

#[test]
fn parse_value_decimal_ok() {
    let v = parse_value_for_kind(ColumnKind::Decimal, "1.5", false, false).unwrap();
    assert!(matches!(v, Some(Value::Text(_))));
    assert_eq!(lit(v.as_ref().unwrap()), "'1.5'");
}

#[test]
fn parse_value_decimal_preserves_precision() {
    let exact = "12345678901234567890.12345678901234567890";
    let value = parse_value_for_kind(ColumnKind::Decimal, exact, false, false).unwrap();
    assert_eq!(lit(value.as_ref().unwrap()), format!("'{exact}'"));
    assert!(parse_value_for_kind(ColumnKind::Decimal, "1.2.3", false, false).is_err());
    assert!(parse_value_for_kind(ColumnKind::Decimal, "1e", false, false).is_err());
}

#[test]
fn parse_value_bool_truthy() {
    for s in ["1", "true", "TRUE", "True"] {
        let v = parse_value_for_kind(ColumnKind::Bool, s, false, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "TRUE", "input={s}");
    }
}

#[test]
fn parse_value_bool_falsy() {
    for s in ["0", "false", "FALSE", "False"] {
        let v = parse_value_for_kind(ColumnKind::Bool, s, false, false).unwrap();
        assert_eq!(lit(v.as_ref().unwrap()), "FALSE", "input={s}");
    }
}

#[test]
fn parse_value_bool_invalid() {
    let err = parse_value_for_kind(ColumnKind::Bool, "yes", false, false).unwrap_err();
    assert!(err.contains("布尔值"));
}

#[test]
fn parse_value_text_trimmed() {
    let v = parse_value_for_kind(ColumnKind::Text, "  hello  ", false, false).unwrap();
    assert_eq!(lit(v.as_ref().unwrap()), "'hello'");
}

/// 构造列元数据（name, nullable, is_pk）
fn make_col(name: &str, nullable: bool, is_pk: bool) -> Column {
    Column {
        name: name.to_string(),
        data_type: ramag_domain::entities::ColumnType {
            kind: ColumnKind::Text,
            raw_type: "text".into(),
        },
        nullable,
        default_value: None,
        is_primary_key: is_pk,
        comment: None,
    }
}

fn make_index(name: &str, unique: bool, primary: bool, cols: &[&str]) -> Index {
    Index {
        name: name.to_string(),
        unique,
        primary,
        columns: cols.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn derive_identity_prefers_real_pk() {
    let cols = vec![
        make_col("uid", false, true),
        make_col("id", true, false), // 名叫 id 但不是主键：绝不能被选中
    ];
    let ident = derive_row_identity(&cols, &[]).unwrap();
    assert_eq!(ident.columns, vec!["uid".to_string()]);
    assert_eq!(ident.label, "主键");
}

#[test]
fn derive_identity_composite_pk() {
    let cols = vec![
        make_col("order_id", false, true),
        make_col("item_id", false, true),
        make_col("qty", false, false),
    ];
    let ident = derive_row_identity(&cols, &[]).unwrap();
    assert_eq!(
        ident.columns,
        vec!["order_id".to_string(), "item_id".to_string()]
    );
}

#[test]
fn derive_identity_falls_back_to_non_null_unique() {
    let cols = vec![
        make_col("email", false, false),
        make_col("name", true, false),
    ];
    let indexes = vec![make_index("uq_email", true, false, &["email"])];
    let ident = derive_row_identity(&cols, &indexes).unwrap();
    assert_eq!(ident.columns, vec!["email".to_string()]);
    assert_eq!(ident.label, "唯一键");
}

#[test]
fn derive_identity_rejects_nullable_unique() {
    // 可空唯一列允许多个 NULL，不能唯一定位行
    let cols = vec![make_col("email", true, false)];
    let indexes = vec![make_index("uq_email", true, false, &["email"])];
    assert!(derive_row_identity(&cols, &indexes).is_none());
}

#[test]
fn derive_identity_none_without_pk_or_unique() {
    let cols = vec![make_col("name", false, false)];
    let indexes = vec![make_index("idx_name", false, false, &["name"])];
    assert!(derive_row_identity(&cols, &indexes).is_none());
}

#[test]
fn build_identity_where_single_pk_mysql() {
    let r = make_result(&["id", "name"]);
    let row = Row {
        values: vec![Value::Int(7), Value::Text("alice".into())],
    };
    let ident = RowIdentity {
        columns: vec!["id".into()],
        label: "主键",
    };
    let s = build_identity_where(&r, &row, &ident, DriverKind::Mysql).unwrap();
    assert_eq!(s, "`id` = 7");
}

#[test]
fn build_identity_where_composite_postgres() {
    let r = make_result(&["order_id", "item_id", "qty"]);
    let row = Row {
        values: vec![Value::Int(1), Value::Int(2), Value::Int(3)],
    };
    let ident = RowIdentity {
        columns: vec!["order_id".into(), "item_id".into()],
        label: "主键",
    };
    let s = build_identity_where(&r, &row, &ident, DriverKind::Postgres).unwrap();
    assert_eq!(s, "\"order_id\" = 1 AND \"item_id\" = 2");
}

#[test]
fn build_identity_where_missing_key_column_returns_error() {
    // 结果集缺键列（如用户只 SELECT 了部分列）：拒绝执行而不是模糊匹配
    let r = make_result(&["name"]);
    let row = Row {
        values: vec![Value::Text("a".into())],
    };
    let ident = RowIdentity {
        columns: vec!["id".into()],
        label: "主键",
    };
    assert_eq!(
        build_identity_where(&r, &row, &ident, DriverKind::Mysql),
        Err(IdentityWhereError::MissingColumn)
    );
}

#[test]
fn build_identity_where_rejects_large_binary_key_before_hex_allocation() {
    let r = make_result(&["id"]);
    let row = Row {
        values: vec![Value::Bytes(vec![0; MAX_SQL_QUERY_BYTES / 2 + 1])],
    };
    let ident = RowIdentity {
        columns: vec!["id".into()],
        label: "主键",
    };

    assert_eq!(
        build_identity_where(&r, &row, &ident, DriverKind::Mysql),
        Err(IdentityWhereError::TooLarge)
    );
}

#[test]
fn dml_row_limit_mysql() {
    assert_eq!(dml_row_limit(DriverKind::Mysql), " LIMIT 1");
}

#[test]
fn dml_row_limit_postgres_empty() {
    assert_eq!(dml_row_limit(DriverKind::Postgres), "");
}

#[test]
fn build_new_value_int_to_int() {
    let v = build_new_value_for_old(&Value::Int(0), "100");
    assert!(matches!(v, Value::Int(100)));
}

#[test]
fn build_new_value_int_to_text_on_parse_fail() {
    let v = build_new_value_for_old(&Value::Int(0), "abc");
    assert_eq!(lit(&v), "'abc'");
}

#[test]
fn build_new_value_null_with_empty_input() {
    let v = build_new_value_for_old(&Value::Null, "");
    assert!(matches!(v, Value::Null));
}

#[test]
fn build_new_value_null_with_text() {
    let v = build_new_value_for_old(&Value::Null, "hello");
    assert_eq!(lit(&v), "'hello'");
}

#[test]
fn batch_delete_notice_reports_misses_and_stale_results() {
    let notice = batch_delete_notice(2, 2, 1, None, "主键", false);

    assert!(notice.message.contains("2 行"));
    assert!(notice.message.contains("1 行未匹配"));
    assert!(notice.message.contains("当前结果已变化"));
    assert!(notice.persistent);
}

#[test]
fn batch_delete_notice_stops_on_multi_row_anomaly() {
    let notice = batch_delete_notice(3, 5, 0, Some(3), "唯一键", true);

    assert!(notice.message.contains("异常影响 3 行"));
    assert!(notice.message.contains("已停止后续删除"));
    assert!(notice.persistent);
}
