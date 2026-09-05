use super::*;
use ramag_domain::entities::{Column, ColumnKind, ColumnType};

fn column(name: &str, raw_type: &str) -> Column {
    Column {
        name: name.into(),
        data_type: ColumnType {
            kind: ColumnKind::Other,
            raw_type: raw_type.into(),
        },
        nullable: true,
        default_value: None,
        is_primary_key: false,
        comment: None,
        ordinal_position: None,
        is_auto_increment: false,
        generation_expression: None,
        generated_storage: None,
        identity_generation: None,
    }
}

#[test]
fn sqlite_migration_emits_supported_column_addition() {
    let source = TableMetadata {
        columns: vec![column("id", "INTEGER"), column("email", "TEXT")],
        ..TableMetadata::default()
    };
    let target = TableMetadata {
        columns: vec![column("id", "INTEGER")],
        ..TableMetadata::default()
    };

    let script = build_migration_script(
        DriverKind::Sqlite,
        "main",
        "users",
        "main",
        "users",
        &source,
        &target,
    )
    .expect("SQLite 新增字段应生成迁移 SQL");

    assert_eq!(script.statement_count, 1);
    assert!(
        script
            .sql
            .contains("ALTER TABLE \"main\".\"users\" ADD COLUMN \"email\" TEXT NULL;")
    );
}

#[test]
fn sqlite_migration_rejects_in_place_column_definition_changes() {
    let source = TableMetadata {
        columns: vec![column("id", "TEXT")],
        ..TableMetadata::default()
    };
    let target = TableMetadata {
        columns: vec![column("id", "INTEGER")],
        ..TableMetadata::default()
    };

    let error = build_migration_script(
        DriverKind::Sqlite,
        "main",
        "users",
        "main",
        "users",
        &source,
        &target,
    )
    .expect_err("SQLite 类型变化必须要求重建表");
    assert!(error.contains("SQLite 字段 id 只支持重命名"));
}
