use super::*;
use ramag_domain::entities::{ColumnKind, ColumnType};

fn pk_col(name: &str) -> Column {
    Column {
        name: name.into(),
        data_type: ColumnType {
            kind: ColumnKind::Integer,
            raw_type: "int".into(),
        },
        nullable: false,
        default_value: None,
        is_primary_key: true,
        comment: None,
        ordinal_position: None,
        is_auto_increment: false,
        generation_expression: None,
        generated_storage: None,
        identity_generation: None,
    }
}

#[test]
fn keyset_select_uses_row_constructor_only_for_composite_pk() {
    let a = pk_col("a");
    let b = pk_col("b");
    let single = vec![&a];
    let composite = vec![&a, &b];
    let first = build_page_select(
        DriverKind::Mysql,
        "`d`.`t`",
        "`a`, `b`",
        &single,
        "`a`",
        &None,
        0,
    );
    assert!(first.contains("ORDER BY `a` LIMIT"));
    assert!(!first.contains("WHERE"));

    let next = build_page_select(
        DriverKind::Mysql,
        "`d`.`t`",
        "`a`, `b`",
        &single,
        "`a`",
        &Some(vec![Value::Int(7)]),
        0,
    );
    assert!(next.contains("WHERE `a` > 7"));

    let composite_next = build_page_select(
        DriverKind::Postgres,
        "\"s\".\"t\"",
        "\"a\", \"b\"",
        &composite,
        "\"a\", \"b\"",
        &Some(vec![Value::Int(1), Value::Text("x".into())]),
        0,
    );
    assert!(composite_next.contains("WHERE (\"a\", \"b\") > (1, 'x')"));
}

#[test]
fn no_pk_select_falls_back_to_offset() {
    let sql = build_page_select(DriverKind::Mysql, "`d`.`t`", "`c`", &[], "", &None, 2000);
    assert!(sql.contains(&format!("LIMIT {PAGE_ROWS} OFFSET 2000")));
}

#[test]
fn definer_clause_is_stripped() {
    let ddl = "CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`local host` SQL SECURITY DEFINER VIEW `v` AS select 1";
    let stripped = strip_mysql_definer(ddl);
    assert!(!stripped.contains("DEFINER="));
    assert!(stripped.contains("CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW"));
    assert_eq!(
        strip_mysql_definer("CREATE VIEW v AS select 1"),
        "CREATE VIEW v AS select 1"
    );
}
