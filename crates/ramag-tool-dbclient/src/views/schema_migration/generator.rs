use ramag_domain::entities::{Column, DriverKind, ForeignKey, ForeignKeyAction, Index};

#[path = "columns.rs"]
mod columns;

pub(super) use columns::{
    append_column_changes, changed_column, has_auto_generation_changes, has_column_changes,
    has_column_order_changes, has_generated_column_changes, has_generated_column_rebuilds,
    has_incomplete_column_metadata,
};

pub(super) struct MigrationStatement {
    pub(super) sql: String,
    pub(super) destructive: bool,
}

pub(super) fn append_foreign_key_drops(
    driver: DriverKind,
    target_name: &str,
    source_columns: &[Column],
    target_columns: &[Column],
    source: &[ForeignKey],
    target: &[ForeignKey],
    statements: &mut Vec<MigrationStatement>,
) -> Result<(), String> {
    for old in target {
        let changed = source
            .iter()
            .find(|new| same_name(&new.name, &old.name))
            .is_none_or(|new| {
                !foreign_key_equivalent(old, new)
                    || old
                        .columns
                        .iter()
                        .any(|column| changed_column(column, source_columns, target_columns))
            });
        if changed {
            let name = identifier(driver, &old.name, "外键名")?;
            let sql = match driver {
                DriverKind::Mysql => format!("ALTER TABLE {target_name} DROP FOREIGN KEY {name};"),
                DriverKind::Postgres => {
                    format!("ALTER TABLE {target_name} DROP CONSTRAINT {name};")
                }
                _ => unreachable!("driver checked by build_migration_script"),
            };
            statements.push(MigrationStatement {
                sql,
                destructive: true,
            });
        }
    }
    Ok(())
}

pub(super) fn append_foreign_key_additions(
    driver: DriverKind,
    target_name: &str,
    source: &[ForeignKey],
    target: &[ForeignKey],
    statements: &mut Vec<MigrationStatement>,
) -> Result<(), String> {
    for new in source {
        let changed = target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_none_or(|old| !foreign_key_equivalent(old, new));
        if changed {
            let sql = foreign_key_add_sql(driver, target_name, new)?;
            statements.push(MigrationStatement {
                sql,
                destructive: false,
            });
        }
    }
    Ok(())
}

/// 生成外键定义；省略默认的 `NO ACTION`，其他动作显式写入 SQL 以保留约束行为。
fn foreign_key_add_sql(
    driver: DriverKind,
    target_name: &str,
    foreign_key: &ForeignKey,
) -> Result<String, String> {
    if foreign_key.columns.is_empty() || foreign_key.columns.len() != foreign_key.ref_columns.len()
    {
        return Err(format!(
            "外键 {} 的字段数量不一致，无法生成迁移 SQL",
            foreign_key.name
        ));
    }
    let name = identifier(driver, &foreign_key.name, "外键名")?;
    let columns = identifiers(driver, &foreign_key.columns, "外键字段")?;
    let ref_columns = identifiers(driver, &foreign_key.ref_columns, "外键引用字段")?;
    let ref_schema = identifier(driver, &foreign_key.ref_schema, "外键引用 Schema")?;
    let ref_table = identifier(driver, &foreign_key.ref_table, "外键引用表")?;
    let mut actions = String::new();
    if foreign_key.on_delete != ForeignKeyAction::NoAction {
        actions.push_str(" ON DELETE ");
        actions.push_str(foreign_key.on_delete.as_sql());
    }
    if foreign_key.on_update != ForeignKeyAction::NoAction {
        actions.push_str(" ON UPDATE ");
        actions.push_str(foreign_key.on_update.as_sql());
    }
    Ok(format!(
        "ALTER TABLE {target_name} ADD CONSTRAINT {name} FOREIGN KEY ({columns}) REFERENCES {ref_schema}.{ref_table} ({ref_columns}){actions};"
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_index_drops(
    driver: DriverKind,
    target_name: &str,
    target_schema: &str,
    source_columns: &[Column],
    target_columns: &[Column],
    source: &[Index],
    target: &[Index],
    statements: &mut Vec<MigrationStatement>,
) -> Result<(), String> {
    for old in target {
        let changed = source
            .iter()
            .find(|new| same_name(&new.name, &old.name))
            .is_none_or(|new| {
                !index_equivalent(old, new)
                    || old
                        .columns
                        .iter()
                        .any(|column| changed_column(column, source_columns, target_columns))
            });
        if changed {
            let sql = index_drop_sql(driver, target_name, target_schema, old)?;
            statements.push(MigrationStatement {
                sql,
                destructive: true,
            });
        }
    }
    Ok(())
}

pub(super) fn append_index_additions(
    driver: DriverKind,
    target_name: &str,
    source_columns: &[Column],
    target_columns: &[Column],
    source: &[Index],
    target: &[Index],
    statements: &mut Vec<MigrationStatement>,
) -> Result<(), String> {
    for new in source {
        let changed = target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_none_or(|old| {
                !index_equivalent(old, new)
                    || new
                        .columns
                        .iter()
                        .any(|column| changed_column(column, source_columns, target_columns))
            });
        if changed {
            let sql = index_add_sql(driver, target_name, new)?;
            statements.push(MigrationStatement {
                sql,
                destructive: false,
            });
        }
    }
    Ok(())
}

fn index_drop_sql(
    driver: DriverKind,
    target_name: &str,
    target_schema: &str,
    index: &Index,
) -> Result<String, String> {
    let name = identifier(driver, &index.name, "索引名")?;
    Ok(match driver {
        DriverKind::Mysql if index.primary => {
            format!("ALTER TABLE {target_name} DROP PRIMARY KEY;")
        }
        DriverKind::Mysql => format!("ALTER TABLE {target_name} DROP INDEX {name};"),
        DriverKind::Postgres if index.primary => {
            format!("ALTER TABLE {target_name} DROP CONSTRAINT {name};")
        }
        DriverKind::Postgres => {
            let schema = identifier(driver, target_schema, "目标 Schema")?;
            format!("DROP INDEX {schema}.{name};")
        }
        _ => unreachable!("driver checked by build_migration_script"),
    })
}

fn index_add_sql(driver: DriverKind, target_name: &str, index: &Index) -> Result<String, String> {
    let name = identifier(driver, &index.name, "索引名")?;
    let columns = index_columns(driver, index)?;
    Ok(match driver {
        DriverKind::Mysql if index.primary => {
            format!("ALTER TABLE {target_name} ADD PRIMARY KEY ({columns});")
        }
        DriverKind::Mysql if index.unique => {
            format!("ALTER TABLE {target_name} ADD UNIQUE INDEX {name} ({columns});")
        }
        DriverKind::Mysql => format!("ALTER TABLE {target_name} ADD INDEX {name} ({columns});"),
        DriverKind::Postgres if index.primary => {
            format!("ALTER TABLE {target_name} ADD CONSTRAINT {name} PRIMARY KEY ({columns});")
        }
        DriverKind::Postgres if index.unique => {
            format!("CREATE UNIQUE INDEX {name} ON {target_name} ({columns});")
        }
        DriverKind::Postgres => format!("CREATE INDEX {name} ON {target_name} ({columns});"),
        _ => unreachable!("driver checked by build_migration_script"),
    })
}

fn index_columns(driver: DriverKind, index: &Index) -> Result<String, String> {
    if index.columns.is_empty() {
        return Err(format!("索引 {} 没有字段，无法生成迁移 SQL", index.name));
    }
    index
        .columns
        .iter()
        .map(|column| {
            if is_simple_identifier(column) {
                identifier(driver, column, "索引字段")
            } else if driver == DriverKind::Postgres {
                fragment(column, "索引表达式")
            } else {
                Err(format!("MySQL 索引字段 {} 不是可安全生成的标识符", column))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|columns| columns.join(", "))
}

pub(super) fn format_script<'a>(
    driver: DriverKind,
    source_name: &str,
    target_name: &str,
    statements: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut output = format!(
        "-- Ramag migration preview\n-- Make target {target_name} match source {source_name}.\n-- Review this script before execution; Ramag runs it only after explicit confirmation.\n"
    );
    let mut has_statement = false;
    for statement in statements {
        has_statement = true;
        output.push('\n');
        output.push_str(statement);
    }
    if !has_statement {
        output.push_str("\n-- No schema changes detected.\n");
    } else if driver == DriverKind::Mysql {
        output.push('\n');
    }
    output
}

pub(super) fn qualified_name(
    driver: DriverKind,
    schema: &str,
    table: &str,
) -> Result<String, String> {
    Ok(format!(
        "{}.{}",
        identifier(driver, schema, "Schema")?,
        identifier(driver, table, "表名")?
    ))
}

fn identifiers(driver: DriverKind, values: &[String], label: &str) -> Result<String, String> {
    values
        .iter()
        .map(|value| identifier(driver, value, label))
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join(", "))
}

fn identifier(driver: DriverKind, value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!("{label}为空或包含控制字符，无法生成迁移 SQL"));
    }
    Ok(driver.quote_identifier(value))
}

fn fragment(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(char::is_control)
        || contains_unquoted_unsafe_token(value)
    {
        return Err(format!("{label}包含无法安全生成的 SQL 片段"));
    }
    Ok(value.to_string())
}

fn contains_unquoted_unsafe_token(value: &str) -> bool {
    let mut quote = None;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if let Some(active) = quote {
            if character == active {
                if chars.peek() == Some(&active) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
        } else if character == ';'
            || (character == '-' && chars.peek() == Some(&'-'))
            || (character == '/' && chars.peek() == Some(&'*'))
        {
            return true;
        }
    }
    quote.is_some()
}

fn escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn normalized_optional(value: Option<&str>) -> &str {
    non_empty(value).unwrap_or("")
}

pub(super) fn same_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn same_names(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_name(left, right))
}

fn index_equivalent(left: &Index, right: &Index) -> bool {
    left.primary == right.primary
        && left.unique == right.unique
        && same_names(&left.columns, &right.columns)
}

pub(super) fn foreign_key_equivalent(left: &ForeignKey, right: &ForeignKey) -> bool {
    same_names(&left.columns, &right.columns)
        && same_name(&left.ref_schema, &right.ref_schema)
        && same_name(&left.ref_table, &right.ref_table)
        && same_names(&left.ref_columns, &right.ref_columns)
        && left.on_delete == right.on_delete
        && left.on_update == right.on_update
}

fn is_simple_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
}
