use ramag_domain::entities::{Column, DriverKind, GeneratedColumnStorage, IdentityGeneration};

use super::{
    MigrationStatement, escape_literal, fragment, identifier, non_empty, non_empty_str,
    normalized_optional, same_name,
};

pub(crate) fn append_column_changes(
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
            for sql in column_add_sql(driver, target_name, source, new)? {
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
        // SQLite exposes ordinal changes in metadata but cannot reorder columns in place.
        if driver == DriverKind::Sqlite && column_definition_equivalent(old, new) {
            continue;
        }
        for sql in column_change_sql(driver, target_name, source, old, new)? {
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
    source: &[Column],
    column: &Column,
) -> Result<Vec<String>, String> {
    if driver == DriverKind::Sqlite && non_empty(column.comment.as_deref()).is_some() {
        return Err(format!("SQLite 字段 {} 不支持迁移字段注释", column.name));
    }
    let definition = column_definition(driver, column, true)?;
    let position = if driver == DriverKind::Mysql {
        mysql_position_clause(source, column)?
    } else {
        String::new()
    };
    if driver == DriverKind::Mysql {
        return Ok(vec![format!(
            "ALTER TABLE {target_name} ADD COLUMN {definition}{position};"
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
    source: &[Column],
    old: &Column,
    new: &Column,
) -> Result<Vec<String>, String> {
    if driver == DriverKind::Mysql {
        let old_name = identifier(driver, &old.name, "字段名")?;
        let definition = column_definition(driver, new, true)?;
        let position = mysql_position_clause(source, new)?;
        return Ok(vec![format!(
            "ALTER TABLE {target_name} CHANGE COLUMN {old_name} {definition}{position};"
        )]);
    }

    let old_name = identifier(driver, &old.name, "字段名")?;
    let new_name = identifier(driver, &new.name, "字段名")?;
    if driver == DriverKind::Sqlite {
        let definition_changed = old.data_type.raw_type != new.data_type.raw_type
            || old.nullable != new.nullable
            || normalized_optional(old.default_value.as_deref())
                != normalized_optional(new.default_value.as_deref())
            || normalized_optional(old.comment.as_deref())
                != normalized_optional(new.comment.as_deref())
            || old.is_primary_key != new.is_primary_key
            || old.is_auto_increment != new.is_auto_increment
            || generated_definition_changed(old, new)
            || old.identity_generation != new.identity_generation;
        if definition_changed {
            return Err(format!(
                "SQLite 字段 {} 只支持重命名；类型、约束或默认值变化请重建表",
                old.name
            ));
        }
        return if old.name != new.name {
            Ok(vec![format!(
                "ALTER TABLE {target_name} RENAME COLUMN {old_name} TO {new_name};"
            )])
        } else {
            Ok(Vec::new())
        };
    }
    if generated_definition_changed(old, new) {
        return postgres_generated_column_rebuild(target_name, old_name, new);
    }

    let mut sql = Vec::new();
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

    let mut default_was_dropped = false;
    if old.identity_generation.is_none()
        && new.identity_generation.is_some()
        && non_empty(old.default_value.as_deref()).is_some()
    {
        sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {new_name} DROP DEFAULT;"
        ));
        default_was_dropped = true;
    }
    append_identity_change(target_name, new_name.as_str(), old, new, &mut sql)?;

    if !default_was_dropped
        && normalized_optional(old.default_value.as_deref())
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

fn postgres_generated_column_rebuild(
    target_name: &str,
    old_name: String,
    new: &Column,
) -> Result<Vec<String>, String> {
    let mut sql = vec![format!("ALTER TABLE {target_name} DROP COLUMN {old_name};")];
    sql.extend(column_add_sql(DriverKind::Postgres, target_name, &[], new)?);
    Ok(sql)
}

fn append_identity_change(
    target_name: &str,
    column_name: &str,
    old: &Column,
    new: &Column,
    sql: &mut Vec<String>,
) -> Result<(), String> {
    if old.is_auto_increment != new.is_auto_increment
        && old.identity_generation == new.identity_generation
    {
        return Err(format!(
            "PostgreSQL 列 {column_name} 的自增属性缺少可执行的 IDENTITY 模式"
        ));
    }
    match (old.identity_generation, new.identity_generation) {
        (None, None) => {}
        (None, Some(mode)) => sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {column_name} ADD GENERATED {} AS IDENTITY;",
            identity_mode(mode)
        )),
        (Some(_), None) => sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {column_name} DROP IDENTITY;"
        )),
        (Some(old_mode), Some(new_mode)) if old_mode != new_mode => sql.push(format!(
            "ALTER TABLE {target_name} ALTER COLUMN {column_name} SET GENERATED {};",
            identity_mode(new_mode)
        )),
        (Some(_), Some(_)) => {}
    }
    Ok(())
}

fn column_definition(
    driver: DriverKind,
    column: &Column,
    include_comment: bool,
) -> Result<String, String> {
    let name = identifier(driver, &column.name, "字段名")?;
    let data_type = fragment(&column.data_type.raw_type, "字段类型")?;
    let expression = non_empty(column.generation_expression.as_deref());
    if expression.is_some() != column.generated_storage.is_some() {
        return Err(format!("字段 {} 的生成列元数据不完整", column.name));
    }
    if column.generated_storage == Some(GeneratedColumnStorage::Virtual)
        && driver == DriverKind::Postgres
    {
        return Err(format!(
            "PostgreSQL 字段 {} 不支持 VIRTUAL 生成列",
            column.name
        ));
    }
    if column.identity_generation.is_some() && expression.is_some() {
        return Err(format!(
            "字段 {} 不能同时声明 IDENTITY 和生成表达式",
            column.name
        ));
    }
    if driver == DriverKind::Mysql && column.identity_generation.is_some() {
        return Err(format!(
            "MySQL 字段 {} 不能使用 PostgreSQL IDENTITY",
            column.name
        ));
    }
    if driver == DriverKind::Sqlite
        && (column.is_primary_key
            || column.is_auto_increment
            || column.identity_generation.is_some())
    {
        return Err(format!(
            "SQLite 新增字段 {} 不能直接声明主键或自增属性",
            column.name
        ));
    }
    if driver == DriverKind::Sqlite
        && include_comment
        && non_empty(column.comment.as_deref()).is_some()
    {
        return Err(format!("SQLite 字段 {} 不支持字段注释", column.name));
    }
    if driver == DriverKind::Postgres
        && column.is_auto_increment
        && column.identity_generation.is_none()
    {
        return Err(format!(
            "PostgreSQL 字段 {} 的自增属性缺少 IDENTITY 模式",
            column.name
        ));
    }
    if (expression.is_some() || column.identity_generation.is_some())
        && non_empty(column.default_value.as_deref()).is_some()
    {
        return Err(format!(
            "字段 {} 不能同时声明默认值和自动生成属性",
            column.name
        ));
    }

    let generated = match (driver, expression, column.generated_storage) {
        (_, None, None) => String::new(),
        (DriverKind::Mysql, Some(expression), Some(storage)) => format!(
            " GENERATED ALWAYS AS ({}) {}",
            fragment(expression, "生成列表达式")?,
            generated_storage(storage)
        ),
        (DriverKind::Postgres, Some(expression), Some(GeneratedColumnStorage::Stored)) => {
            format!(
                " GENERATED ALWAYS AS ({}) STORED",
                fragment(expression, "生成列表达式")?
            )
        }
        (DriverKind::Sqlite, Some(expression), Some(storage)) => format!(
            " GENERATED ALWAYS AS ({}) {}",
            fragment(expression, "生成列表达式")?,
            generated_storage(storage)
        ),
        _ => {
            return Err(format!(
                "字段 {} 的生成列存储方式不受当前数据库支持",
                column.name
            ));
        }
    };
    let identity = match (driver, column.identity_generation) {
        (_, None) => String::new(),
        (DriverKind::Postgres, Some(mode)) => {
            format!(" GENERATED {} AS IDENTITY", identity_mode(mode))
        }
        (DriverKind::Mysql, Some(_)) => unreachable!("MySQL identity was rejected above"),
        (_, Some(_)) => {
            return Err(format!(
                "字段 {} 的 IDENTITY 属性不受当前数据库支持",
                column.name
            ));
        }
    };
    let nullability = if column.nullable {
        " NULL"
    } else {
        " NOT NULL"
    };
    let default = match non_empty(column.default_value.as_deref()) {
        Some(value) => format!(" DEFAULT {}", fragment(value, "默认值")?),
        None => String::new(),
    };
    let auto_increment = if driver == DriverKind::Mysql && column.is_auto_increment {
        " AUTO_INCREMENT"
    } else {
        ""
    };
    let comment = if include_comment && driver == DriverKind::Mysql {
        match column.comment.as_deref().and_then(non_empty_str) {
            Some(value) => format!(" COMMENT '{}'", escape_literal(value)),
            None => String::new(),
        }
    } else {
        String::new()
    };
    Ok(format!(
        "{name} {data_type}{generated}{identity}{nullability}{default}{auto_increment}{comment}"
    ))
}

fn mysql_position_clause(source: &[Column], column: &Column) -> Result<String, String> {
    let Some(position) = column.ordinal_position else {
        return Ok(String::new());
    };
    match position {
        0 => Err(format!("字段 {} 的列序号无效", column.name)),
        1 => Ok(" FIRST".to_string()),
        position => {
            let previous = source
                .iter()
                .find(|candidate| candidate.ordinal_position == Some(position - 1))
                .ok_or_else(|| format!("字段 {} 的前置列序号不连续", column.name))?;
            Ok(format!(
                " AFTER {}",
                identifier(DriverKind::Mysql, &previous.name, "前置字段")?
            ))
        }
    }
}

fn generated_storage(storage: GeneratedColumnStorage) -> &'static str {
    match storage {
        GeneratedColumnStorage::Virtual => "VIRTUAL",
        GeneratedColumnStorage::Stored => "STORED",
    }
}

fn identity_mode(mode: IdentityGeneration) -> &'static str {
    match mode {
        IdentityGeneration::Always => "ALWAYS",
        IdentityGeneration::ByDefault => "BY DEFAULT",
    }
}

fn generated_definition_changed(left: &Column, right: &Column) -> bool {
    normalized_optional(left.generation_expression.as_deref())
        != normalized_optional(right.generation_expression.as_deref())
        || left.generated_storage != right.generated_storage
}

fn column_equivalent(left: &Column, right: &Column) -> bool {
    column_definition_equivalent(left, right) && positions_equal(left, right)
}

fn column_definition_equivalent(left: &Column, right: &Column) -> bool {
    left.name == right.name
        && left.data_type.raw_type == right.data_type.raw_type
        && left.nullable == right.nullable
        && normalized_optional(left.default_value.as_deref())
            == normalized_optional(right.default_value.as_deref())
        && normalized_optional(left.comment.as_deref())
            == normalized_optional(right.comment.as_deref())
        && left.is_auto_increment == right.is_auto_increment
        && normalized_optional(left.generation_expression.as_deref())
            == normalized_optional(right.generation_expression.as_deref())
        && left.generated_storage == right.generated_storage
        && left.identity_generation == right.identity_generation
}

fn positions_equal(left: &Column, right: &Column) -> bool {
    match (left.ordinal_position, right.ordinal_position) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

pub(crate) fn has_column_changes(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_none_or(|old| !column_equivalent(old, new))
    }) || target
        .iter()
        .any(|old| !source.iter().any(|new| same_name(&new.name, &old.name)))
}

pub(crate) fn has_column_order_changes(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_some_and(|old| !positions_equal(old, new))
    })
}

pub(crate) fn has_generated_column_changes(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_some_and(|old| {
                normalized_optional(old.generation_expression.as_deref())
                    != normalized_optional(new.generation_expression.as_deref())
                    || old.generated_storage != new.generated_storage
            })
            || (!target.iter().any(|old| same_name(&old.name, &new.name))
                && (new.generation_expression.is_some() || new.generated_storage.is_some()))
    }) || target.iter().any(|old| {
        !source.iter().any(|new| same_name(&new.name, &old.name))
            && (old.generation_expression.is_some() || old.generated_storage.is_some())
    })
}

pub(crate) fn has_generated_column_rebuilds(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_some_and(|old| generated_definition_changed(old, new))
    })
}

pub(crate) fn has_auto_generation_changes(source: &[Column], target: &[Column]) -> bool {
    source.iter().any(|new| {
        target
            .iter()
            .find(|old| same_name(&old.name, &new.name))
            .is_some_and(|old| {
                old.is_auto_increment != new.is_auto_increment
                    || old.identity_generation != new.identity_generation
            })
            || (!target.iter().any(|old| same_name(&old.name, &new.name))
                && (new.is_auto_increment || new.identity_generation.is_some()))
    }) || target.iter().any(|old| {
        !source.iter().any(|new| same_name(&new.name, &old.name))
            && (old.is_auto_increment || old.identity_generation.is_some())
    })
}

pub(crate) fn has_incomplete_column_metadata(source: &[Column], target: &[Column]) -> bool {
    source
        .iter()
        .chain(target)
        .any(|column| column.ordinal_position.is_none())
}

pub(crate) fn changed_column(name: &str, source: &[Column], target: &[Column]) -> bool {
    let Some(old) = target.iter().find(|column| same_name(&column.name, name)) else {
        return false;
    };
    source
        .iter()
        .find(|column| same_name(&column.name, name))
        .is_none_or(|new| !column_definition_equivalent(old, new))
}
