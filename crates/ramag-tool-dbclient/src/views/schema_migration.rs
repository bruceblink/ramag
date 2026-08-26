//! 根据两张 SQL 表的元数据生成只读迁移脚本。

use ramag_domain::entities::DriverKind;

use super::schema_diff::TableMetadata;

mod generator;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MigrationScript {
    pub(crate) sql: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) statement_count: usize,
    pub(crate) destructive_statements: usize,
}

/// 生成将目标表调整为源表结构的 SQL；此函数只读取元数据，不执行 SQL。
pub(crate) fn build_migration_script(
    driver: DriverKind,
    source_schema: &str,
    source_table: &str,
    target_schema: &str,
    target_table: &str,
    source: &TableMetadata,
    target: &TableMetadata,
) -> Result<MigrationScript, String> {
    if !matches!(driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err("当前数据库不支持生成表结构迁移 SQL".into());
    }
    let source_name = generator::qualified_name(driver, source_schema, source_table)?;
    let target_name = generator::qualified_name(driver, target_schema, target_table)?;
    let mut statements = Vec::new();
    let mut warnings = Vec::new();

    // 先删除会阻止列变更的外键和索引，再处理列，最后恢复新增对象。
    generator::append_foreign_key_drops(
        driver,
        &target_name,
        &source.columns,
        &target.columns,
        &source.foreign_keys,
        &target.foreign_keys,
        &mut statements,
    )?;
    generator::append_index_drops(
        driver,
        &target_name,
        target_schema,
        &source.columns,
        &target.columns,
        &source.indexes,
        &target.indexes,
        &mut statements,
    )?;
    generator::append_column_changes(
        driver,
        &target_name,
        &source.columns,
        &target.columns,
        &mut statements,
    )?;
    generator::append_index_additions(
        driver,
        &target_name,
        &source.columns,
        &target.columns,
        &source.indexes,
        &target.indexes,
        &mut statements,
    )?;
    generator::append_foreign_key_additions(
        driver,
        &target_name,
        &source.foreign_keys,
        &target.foreign_keys,
        &mut statements,
    )?;

    if generator::has_column_changes(&source.columns, &target.columns) {
        warnings.push("列顺序、自动生成属性和自增属性不在当前元数据中，执行前请人工复核".into());
    }
    if source
        .indexes
        .iter()
        .any(|index| index.unique && !index.primary)
    {
        warnings
            .push("唯一索引与唯一约束在当前元数据中无法区分，PostgreSQL 将按唯一索引生成".into());
    }
    if driver == DriverKind::Mysql && !statements.is_empty() {
        warnings
            .push("MySQL DDL 可能隐式提交；执行失败时目标表可能已部分变更，请先人工复核".into());
    }

    let destructive_statements = statements
        .iter()
        .filter(|statement| statement.destructive)
        .count();
    let sql = generator::format_script(
        driver,
        &source_name,
        &target_name,
        statements.iter().map(|statement| statement.sql.as_str()),
    );
    Ok(MigrationScript {
        statement_count: statements.len(),
        destructive_statements,
        sql,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::entities::{
        Column, ColumnKind, ColumnType, ForeignKey, ForeignKeyAction, Index,
    };

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
        }
    }

    fn index(name: &str, columns: &[&str]) -> Index {
        Index {
            name: name.into(),
            unique: false,
            primary: false,
            columns: columns.iter().map(|column| (*column).into()).collect(),
        }
    }

    #[test]
    fn mysql_migration_drops_dependencies_before_columns_and_restores_them() {
        let source = TableMetadata {
            columns: vec![column("id", "INT"), column("email", "VARCHAR(255)")],
            indexes: vec![Index {
                primary: true,
                name: "PRIMARY".into(),
                ..index("PRIMARY", &["id"])
            }],
            foreign_keys: vec![],
        };
        let target = TableMetadata {
            columns: vec![column("id", "BIGINT"), column("legacy", "TEXT")],
            indexes: vec![Index {
                primary: true,
                name: "PRIMARY".into(),
                ..index("PRIMARY", &["id"])
            }],
            foreign_keys: vec![],
        };
        let script = build_migration_script(
            DriverKind::Mysql,
            "app",
            "users",
            "app",
            "users",
            &source,
            &target,
        )
        .expect("migration script");
        let drop_index = script
            .sql
            .find("DROP PRIMARY KEY")
            .expect("drop primary key");
        let drop_column = script
            .sql
            .find("DROP COLUMN `legacy`")
            .expect("drop column");
        let add_column = script.sql.find("ADD COLUMN `email`").expect("add column");
        let add_index = script
            .sql
            .rfind("ADD PRIMARY KEY")
            .expect("add primary key");
        assert!(drop_index < drop_column);
        assert!(drop_column < add_column);
        assert!(add_column < add_index);
        assert_eq!(script.destructive_statements, 3);
        assert!(
            script
                .warnings
                .iter()
                .any(|warning| warning.contains("列顺序"))
        );
    }

    #[test]
    fn postgres_migration_quotes_names_and_emits_comment_changes() {
        let mut old = column("UserName", "text");
        old.comment = Some("old".into());
        let mut new = column("username", "text");
        new.comment = Some("owner's email".into());
        let source = TableMetadata {
            columns: vec![new],
            ..TableMetadata::default()
        };
        let target = TableMetadata {
            columns: vec![old],
            ..TableMetadata::default()
        };
        let script = build_migration_script(
            DriverKind::Postgres,
            "sales data",
            "users",
            "sales data",
            "users",
            &source,
            &target,
        )
        .expect("migration script");
        assert!(script.sql.contains(
            "ALTER TABLE \"sales data\".\"users\" RENAME COLUMN \"UserName\" TO \"username\";"
        ));
        assert!(script.sql.contains("IS 'owner''s email';"));
    }

    #[test]
    fn unchanged_metadata_reports_no_statements() {
        let source = TableMetadata {
            columns: vec![column("id", "int")],
            ..TableMetadata::default()
        };
        let script = build_migration_script(
            DriverKind::Mysql,
            "app",
            "users",
            "app",
            "users",
            &source,
            &source,
        )
        .expect("migration script");
        assert_eq!(script.statement_count, 0);
        assert!(script.sql.contains("No schema changes detected"));
    }

    #[test]
    fn unsafe_type_is_rejected_before_script_generation() {
        let source = TableMetadata {
            columns: vec![column("id", "INT; DROP TABLE users")],
            ..TableMetadata::default()
        };
        let error = build_migration_script(
            DriverKind::Mysql,
            "app",
            "users",
            "app",
            "users",
            &source,
            &TableMetadata::default(),
        )
        .expect_err("unsafe type should be rejected");
        assert!(error.contains("字段类型"));
    }

    #[test]
    fn migration_preserves_foreign_key_actions() {
        let source = TableMetadata {
            foreign_keys: vec![ForeignKey {
                name: "fk_project".into(),
                columns: vec!["project_id".into()],
                ref_schema: "app".into(),
                ref_table: "projects".into(),
                ref_columns: vec!["id".into()],
                on_delete: ForeignKeyAction::Cascade,
                on_update: ForeignKeyAction::SetNull,
            }],
            ..TableMetadata::default()
        };
        let target = TableMetadata {
            foreign_keys: vec![ForeignKey {
                on_delete: ForeignKeyAction::NoAction,
                on_update: ForeignKeyAction::NoAction,
                ..source.foreign_keys[0].clone()
            }],
            ..TableMetadata::default()
        };

        let script = build_migration_script(
            DriverKind::Postgres,
            "app",
            "projects",
            "app",
            "projects_copy",
            &source,
            &target,
        )
        .expect("migration script");
        assert!(script.sql.contains("ON DELETE CASCADE ON UPDATE SET NULL"));
        assert!(
            !script
                .warnings
                .iter()
                .any(|warning| warning.contains("外键动作"))
        );
    }
}
