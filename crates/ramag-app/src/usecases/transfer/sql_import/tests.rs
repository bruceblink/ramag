use super::*;

#[test]
fn segment_kinds_parse_and_classify() {
    assert_eq!(SegmentKind::parse("table"), Some(SegmentKind::Table));
    assert_eq!(
        SegmentKind::parse("sequences-pre"),
        Some(SegmentKind::SequencesPre)
    );
    assert_eq!(SegmentKind::parse("bogus"), None);
    assert!(SegmentKind::Fk.tolerates_errors());
    assert!(!SegmentKind::Data.tolerates_errors());
}

#[test]
fn merge_rewrites_insert_statements_per_dialect() {
    let line = "INSERT INTO `t` (`a`) VALUES (1), (2);\n";
    assert_eq!(
        merge_rewrite_line(line, DriverKind::Mysql).as_deref(),
        Some("INSERT IGNORE INTO `t` (`a`) VALUES (1), (2);\n")
    );
    let pg = "INSERT INTO \"s\".\"t\" (\"a\") OVERRIDING SYSTEM VALUE VALUES (1);\n";
    assert_eq!(
        merge_rewrite_line(pg, DriverKind::Postgres).as_deref(),
        Some(
            "INSERT INTO \"s\".\"t\" (\"a\") OVERRIDING SYSTEM VALUE VALUES (1) ON CONFLICT DO NOTHING;\n"
        )
    );
    assert_eq!(
        merge_rewrite_line("CREATE TABLE t (a int);\n", DriverKind::Mysql),
        None
    );
    assert_eq!(
        merge_rewrite_line("INSERT INTO t VALUES (1)", DriverKind::Postgres),
        None
    );
}

#[test]
fn use_lines_are_recognized() {
    assert!(is_use_statement("USE `shop`;"));
    assert!(is_use_statement("  use x;  "));
    assert!(!is_use_statement("USE `shop`"));
    assert!(!is_use_statement("SELECT usedata FROM t;"));
    assert!(!is_use_statement("-- use note;"));
}

#[test]
fn table_import_header_requires_single_table_export_for_current_database() {
    let valid = "-- ramag table export v1\n\
                     -- engine: postgres\n\
                     -- database: public\n\
                     -- table: users\n\
                     -- ramag:begin header\n";
    assert_eq!(
        parse_table_export_header(std::io::Cursor::new(valid), DriverKind::Postgres, "public")
            .unwrap(),
        "users"
    );
    assert!(
        parse_table_export_header(std::io::Cursor::new(valid), DriverKind::Postgres, "archive")
            .is_err()
    );

    let database_export = valid.replacen("table export", "database export", 1);
    assert!(
        parse_table_export_header(
            std::io::Cursor::new(database_export),
            DriverKind::Postgres,
            "public"
        )
        .is_err()
    );
}
