//! 索引和触发器的方言 SQL 构造与输入校验。

use ramag_domain::entities::{DriverKind, Index, Trigger};
use ramag_infra_sql_shared::sql::{SplitOptions, split_statements_bounded};

pub(super) fn index_create_sql(
    driver: DriverKind,
    schema: &str,
    table: &str,
    index: &Index,
) -> Result<String, String> {
    if index.primary {
        return Err("主键不能从索引菜单更新".into());
    }
    let name = identifier(driver, &index.name, "索引名")?;
    let table = qualified_name(driver, schema, table)?;
    let columns = index_columns(driver, index)?;
    let unique = if index.unique { "UNIQUE " } else { "" };
    match driver {
        DriverKind::Mysql => Ok(format!(
            "ALTER TABLE {table} ADD {unique}INDEX {name} ({columns})"
        )),
        DriverKind::Postgres => Ok(format!(
            "CREATE {unique}INDEX {name} ON {table} ({columns})"
        )),
        _ => Err("当前数据库类型不支持索引操作".into()),
    }
}

pub(super) fn index_drop_sql(
    driver: DriverKind,
    schema: &str,
    table: &str,
    index: &Index,
) -> Result<String, String> {
    if index.primary {
        return Err("主键不能从索引菜单删除".into());
    }
    let name = identifier(driver, &index.name, "索引名")?;
    let table = qualified_name(driver, schema, table)?;
    match driver {
        DriverKind::Mysql => Ok(format!("ALTER TABLE {table} DROP INDEX {name}")),
        DriverKind::Postgres => Ok(format!(
            "DROP INDEX {}.{}",
            identifier(driver, schema, "Schema")?,
            name
        )),
        _ => Err("当前数据库类型不支持索引操作".into()),
    }
}

pub(super) fn trigger_create_sql(
    driver: DriverKind,
    schema: &str,
    table: &str,
    trigger: &Trigger,
) -> Result<String, String> {
    let definition = trigger.definition.trim();
    if definition.is_empty() {
        return Err(format!("触发器 {} 没有可编辑的定义", trigger.name));
    }
    match driver {
        DriverKind::Mysql => {
            let name = identifier(driver, &trigger.name, "触发器名")?;
            let table = qualified_name(driver, schema, table)?;
            let timing = trigger_part(&trigger.timing, "触发时机", &["BEFORE", "AFTER"])?;
            let event = trigger_part(&trigger.event, "触发事件", &["INSERT", "UPDATE", "DELETE"])?;
            Ok(format!(
                "CREATE TRIGGER {name} {timing} {event} ON {table} FOR EACH ROW {definition}"
            ))
        }
        DriverKind::Postgres if is_trigger_create_sql(driver, definition) => {
            Ok(definition.to_string())
        }
        DriverKind::Postgres => Err("PostgreSQL 元数据未返回完整的 CREATE TRIGGER 定义".into()),
        _ => Err("当前数据库类型不支持触发器操作".into()),
    }
}

pub(super) fn trigger_drop_sql(
    driver: DriverKind,
    schema: &str,
    table: &str,
    name: &str,
) -> Result<String, String> {
    let name = identifier(driver, name, "触发器名")?;
    match driver {
        DriverKind::Mysql => Ok(format!(
            "DROP TRIGGER {}.{}",
            identifier(driver, schema, "Schema")?,
            name
        )),
        DriverKind::Postgres => Ok(format!(
            "DROP TRIGGER {name} ON {}",
            qualified_name(driver, schema, table)?
        )),
        _ => Err("当前数据库类型不支持触发器操作".into()),
    }
}

fn identifier(driver: DriverKind, value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!("{label}为空或包含控制字符"));
    }
    Ok(driver.quote_identifier(value))
}

fn qualified_name(driver: DriverKind, schema: &str, table: &str) -> Result<String, String> {
    Ok(format!(
        "{}.{}",
        identifier(driver, schema, "Schema")?,
        identifier(driver, table, "表名")?
    ))
}

fn index_columns(driver: DriverKind, index: &Index) -> Result<String, String> {
    if index.columns.is_empty() {
        return Err(format!("索引 {} 没有字段", index.name));
    }
    index
        .columns
        .iter()
        .map(|column| {
            if is_simple_identifier(column) {
                identifier(driver, column, "索引字段")
            } else if driver == DriverKind::Postgres && is_safe_fragment(column) {
                Ok(column.trim().to_string())
            } else {
                Err(format!("索引字段 {} 不是可安全生成的 SQL", column))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|columns| columns.join(", "))
}

fn trigger_part(value: &str, label: &str, allowed: &[&str]) -> Result<String, String> {
    let value = value.trim();
    allowed
        .iter()
        .find(|candidate| value.eq_ignore_ascii_case(candidate))
        .map(|candidate| (*candidate).to_string())
        .ok_or_else(|| format!("{label} {value} 不受支持"))
}

fn is_simple_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$'))
}

fn is_safe_fragment(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
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
            return false;
        }
    }
    quote.is_none()
}

pub(super) fn is_index_create_sql(driver: DriverKind, sql: &str) -> bool {
    is_single_statement(driver, sql, is_index_create_statement)
}

fn is_index_create_statement(sql: &str) -> bool {
    has_prefix(sql, "CREATE INDEX")
        || has_prefix(sql, "CREATE UNIQUE INDEX")
        || (has_prefix(sql, "ALTER TABLE") && has_index_add_clause(sql))
}

pub(super) fn is_trigger_create_sql(driver: DriverKind, sql: &str) -> bool {
    is_single_statement(driver, sql, is_trigger_create_statement)
}

fn is_trigger_create_statement(sql: &str) -> bool {
    let mut words = sql.split_whitespace();
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("CREATE"))
    {
        return false;
    }
    match words.next() {
        Some(word) if word.eq_ignore_ascii_case("TRIGGER") => true,
        Some(word) if word.eq_ignore_ascii_case("OR") => {
            words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("REPLACE"))
                && words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("TRIGGER"))
        }
        Some(word) if word.eq_ignore_ascii_case("CONSTRAINT") => words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("TRIGGER")),
        Some(word) if word.to_ascii_uppercase().starts_with("DEFINER") => words
            .take(8)
            .any(|word| word.eq_ignore_ascii_case("TRIGGER")),
        _ => false,
    }
}

fn is_single_statement(
    driver: DriverKind,
    sql: &str,
    predicate: impl FnOnce(&str) -> bool,
) -> bool {
    let options = match driver {
        DriverKind::Mysql => SplitOptions::mysql(),
        DriverKind::Postgres => SplitOptions::postgres(),
        DriverKind::Redis | DriverKind::Mongodb => return false,
    };
    let Ok(statements) = split_statements_bounded(sql, options, 1) else {
        return false;
    };
    statements.len() == 1 && predicate(&statements[0])
}

fn has_index_add_clause(sql: &str) -> bool {
    let words = sql
        .to_ascii_uppercase()
        .split_whitespace()
        .map(|word| word.trim_matches(|character: char| !character.is_ascii_alphabetic()))
        .collect::<Vec<_>>();
    let Some(add) = words.iter().position(|word| *word == "ADD") else {
        return false;
    };
    let rest = &words[add + 1..];
    match rest {
        [first, ..] if matches!(*first, "INDEX" | "KEY") => true,
        [first, second, ..]
            if matches!(*first, "UNIQUE" | "FULLTEXT" | "SPATIAL")
                && matches!(*second, "INDEX" | "KEY") =>
        {
            true
        }
        [constraint, _, unique, rest @ ..]
            if *constraint == "CONSTRAINT"
                && *unique == "UNIQUE"
                && (rest.is_empty() || matches!(rest[0], "INDEX" | "KEY")) =>
        {
            true
        }
        _ => false,
    }
}

fn has_prefix(sql: &str, prefix: &str) -> bool {
    let value = sql.trim_start();
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        && value
            .as_bytes()
            .get(prefix.len())
            .is_some_and(u8::is_ascii_whitespace)
}

pub(super) fn combine_ddl(drop_sql: &str, create_sql: &str) -> String {
    format!(
        "{};\n{}",
        trim_statement(drop_sql),
        trim_statement(create_sql)
    )
}

fn trim_statement(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim_end()
}

#[cfg(test)]
mod tests {
    use super::{
        combine_ddl, index_create_sql, is_index_create_sql, is_trigger_create_sql,
        trigger_create_sql, trigger_drop_sql,
    };
    use ramag_domain::entities::{DriverKind, Index, Trigger};

    #[test]
    fn index_sql_uses_driver_specific_forms() {
        let index = Index {
            name: "idx_order_status".into(),
            unique: false,
            primary: false,
            columns: vec!["status".into()],
        };
        assert_eq!(
            index_create_sql(DriverKind::Mysql, "shop", "orders", &index).unwrap(),
            "ALTER TABLE `shop`.`orders` ADD INDEX `idx_order_status` (`status`)"
        );
        assert_eq!(
            index_create_sql(DriverKind::Postgres, "public", "orders", &index).unwrap(),
            "CREATE INDEX \"idx_order_status\" ON \"public\".\"orders\" (\"status\")"
        );
    }

    #[test]
    fn trigger_sql_keeps_mysql_body_and_postgres_definition() {
        let mysql = Trigger {
            name: "orders_audit".into(),
            timing: "AFTER".into(),
            event: "INSERT".into(),
            definition: "SET @audit = 1".into(),
        };
        assert_eq!(
            trigger_create_sql(DriverKind::Mysql, "shop", "orders", &mysql).unwrap(),
            "CREATE TRIGGER `orders_audit` AFTER INSERT ON `shop`.`orders` FOR EACH ROW SET @audit = 1"
        );

        let postgres = Trigger {
            definition: "CREATE TRIGGER orders_audit AFTER INSERT ON public.orders FOR EACH ROW EXECUTE FUNCTION audit_order()".into(),
            ..mysql
        };
        assert_eq!(
            trigger_create_sql(DriverKind::Postgres, "public", "orders", &postgres).unwrap(),
            postgres.definition
        );
    }

    #[test]
    fn trigger_drop_sql_places_postgres_table_after_trigger_name() {
        assert_eq!(
            trigger_drop_sql(DriverKind::Mysql, "shop", "orders", "orders_audit").unwrap(),
            "DROP TRIGGER `shop`.`orders_audit`"
        );
        assert_eq!(
            trigger_drop_sql(DriverKind::Postgres, "public", "orders", "orders_audit").unwrap(),
            "DROP TRIGGER \"orders_audit\" ON \"public\".\"orders\""
        );
    }

    #[test]
    fn combine_ddl_removes_only_terminal_separators() {
        assert_eq!(
            combine_ddl("DROP INDEX old;", "CREATE INDEX new ON t (id);"),
            "DROP INDEX old;\nCREATE INDEX new ON t (id)"
        );
    }

    #[test]
    fn update_validation_accepts_index_and_trigger_ddl_only() {
        assert!(is_index_create_sql(
            DriverKind::Mysql,
            "ALTER TABLE `shop`.`orders` ADD UNIQUE INDEX `idx_status` (`status`)"
        ));
        assert!(!is_index_create_sql(
            DriverKind::Mysql,
            "ALTER TABLE `shop`.`orders` ADD COLUMN `status_copy` varchar(32)"
        ));
        assert!(is_trigger_create_sql(
            DriverKind::Mysql,
            "CREATE DEFINER=`admin`@`%` TRIGGER `audit` AFTER INSERT ON `orders` FOR EACH ROW SET @seen = 1"
        ));
        assert!(is_trigger_create_sql(
            DriverKind::Postgres,
            "CREATE OR REPLACE TRIGGER audit AFTER INSERT ON orders FOR EACH ROW EXECUTE FUNCTION audit_order()"
        ));
        assert!(is_trigger_create_sql(
            DriverKind::Postgres,
            "CREATE CONSTRAINT TRIGGER audit AFTER INSERT ON orders DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION audit_order()"
        ));
        assert!(!is_trigger_create_sql(
            DriverKind::Postgres,
            "CREATE TABLE audit(id int)"
        ));
        assert!(!is_index_create_sql(
            DriverKind::Mysql,
            "CREATE INDEX idx_status ON orders (status); DROP TABLE orders"
        ));
        assert!(!is_trigger_create_sql(
            DriverKind::Mysql,
            "CREATE TRIGGER audit AFTER INSERT ON orders FOR EACH ROW BEGIN SET @seen = 1; END; DROP TABLE orders"
        ));
    }
}
