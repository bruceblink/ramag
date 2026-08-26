use ramag_domain::entities::{Column, DriverKind, ForeignKey, Index};

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
    Ok(format!(
        "ALTER TABLE {target_name} ADD CONSTRAINT {name} FOREIGN KEY ({columns}) REFERENCES {ref_schema}.{ref_table} ({ref_columns});"
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

pub(super) fn append_column_changes(
    driver: DriverKind,
    target_name: &str,
    source: &[Column],
    target: &[Column],
    statements: &mut Vec<MigrationStatement>,
) -> Result<(), String> {
    for old in target {
        if !source.iter().any(|new| same_name(&new.name, &old.name)) {
            let name = identifier(driver, &old.name, "字段名")?;
            statements.push(MigrationStatement {
                sql: format!("ALTER TABLE {target_name} DROP COLUMN {name};"),
                destructive: true,
            });
        }
    }

    for new in source {
        let Some(old) = target.iter().find(|old| same_name(&old.name, &new.name)) else {
            for sql in column_add_sql(driver, target_name, new)? {
                statements.push(MigrationStatement {
                    sql,
                    destructive: false,
                });
            }
            continue;
        };
        if column_equivalent(old, new) {
            continue;
        }
        for sql in column_change_sql(driver, target_name, old, new)? {
            statements.push(MigrationStatement {
                sql,
                destructive: true,
            });
        }
    }
    Ok(())
}

fn column_add_sql(
    driver: DriverKind,
    target_name: &str,
    column: &Column,
) -> Result<Vec<String>, String> {
    let definition = column_definition(driver, column, true)?;
    if driver == DriverKind::Mysql {
        return Ok(vec![format!(
            "ALTER TABLE {target_name} ADD COLUMN {definition};"
        )]);
    }

    let mut sql = vec![format!(
        "ALTER TABLE {target_name} ADD COLUMN {definition};"
    )];
    if let Some(comment) = non_empty(column.comment.as_deref()) {
        let name = identifier(driver, &column.name, "字段名")?;
        sql.push(format!(
            "COMMENT ON COLUMN {target_name}.{name} IS '{}';",
            escape_literal(comment)
        ));
    }
    Ok(sql)
}

fn column_change_sql(
    driver: DriverKind,
    target_name: &str,
    old: &Column,
    new: &Column,
) -> Result<Vec<String>, String> {
    if driver == DriverKind::Mysql {
        let old_name = identifier(driver, &old.name, "字段名")?;
        let definition = column_definition(driver, new, true)?;
        return Ok(vec![format!(
            "ALTER TABLE {target_name} CHANGE COLUMN {old_name} {definition};"
        )]);
    }

    let mut sql = Vec::new();
    let old_name = identifier(driver, &old.name, "字段名")?;
    let new_name = identifier(driver, &new.name, "字段名")?;
    if old.name != new.name {
        sql.push(format!(
            "ALTER TABLE {target_name} RENAME COLUMN {old_name} TO {new_name};"
        ));
    }
    if old.data_type.raw_type != new.data_type.raw_type {
        let data_type = fragment(&new.data_type.raw_type, "字段类型")?;
        sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {new_name} TYPE {data_type};"
        ));
    }
    if old.nullable != new.nullable {
        sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {new_name} {};",
            if new.nullable {
                "DROP NOT NULL"
            } else {
                "SET NOT NULL"
            }
        ));
    }
    if normalized_optional(old.default_value.as_deref())
        != normalized_optional(new.default_value.as_deref())
    {
        let clause = match non_empty(new.default_value.as_deref()) {
            Some(value) => format!("SET DEFAULT {}", fragment(value, "默认值")?),
            None => "DROP DEFAULT".to_string(),
        };
        sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {new_name} {clause};"
        ));
    }
    if normalized_optional(old.comment.as_deref()) != normalized_optional(new.comment.as_deref()) {
        let value = new.comment.as_deref().and_then(non_empty_str).map_or_else(
            || "NULL".to_string(),
            |comment| format!("'{}'", escape_literal(comment)),
        );
        sql.push(format!(
            "COMMENT ON COLUMN {target_name}.{new_name} IS {value};"
        ));
    }
    Ok(sql)
}

fn column_definition(
    driver: DriverKind,
    column: &Column,
    include_comment: bool,
) -> Result<String, String> {
    let name = identifier(driver, &column.name, "字段名")?;
    let data_type = fragment(&column.data_type.raw_type, "字段类型")?;
    let nullability = if column.nullable {
        " NULL"
    } else {
        " NOT NULL"
    };
    let default = match non_empty(column.default_value.as_deref()) {
        Some(value) => format!(" DEFAULT {}", fragment(value, "默认值")?),
        None => String::new(),
    };
    let comment = if include_comment && driver == DriverKind::Mysql {
        match column.comment.as_deref().and_then(non_empty_str) {
            Some(value) => format!(" COMMENT '{}'", escape_literal(value)),
            None => String::new(),
        }
    } else {
        String::new()
    };
    Ok(format!("{name} {data_type}{nullability}{default}{comment}"))
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

fn column_equivalent(left: &Column, right: &Column) -> bool {
    left.name == right.name
        && left.data_type.raw_type == right.data_type.raw_type
        && left.nullable == right.nullable
        && normalized_optional(left.default_value.as_deref())
            == normalized_optional(right.default_value.as_deref())
        && normalized_optional(left.comment.as_deref())
            == normalized_optional(right.comment.as_deref())
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
}

pub(super) fn has_column_changes(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_none_or(|old| !column_equivalent(old, new))
    }) || target
        .iter()
        .any(|old| !source.iter().any(|new| same_name(&new.name, &old.name)))
}

fn changed_column(name: &str, source: &[Column], target: &[Column]) -> bool {
    let Some(old) = target.iter().find(|column| same_name(&column.name, name)) else {
        return false;
    };
    source
        .iter()
        .find(|column| same_name(&column.name, name))
        .is_none_or(|new| !column_equivalent(old, new))
}

fn is_simple_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
}
