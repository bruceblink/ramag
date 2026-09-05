//! 表级 JSONL 导入的 SQL 转义与拼接。

use ramag_domain::entities::{ConflictPolicy, DriverKind};

/// 按列顺序渲染一行 `VALUES` 元组。
pub(super) fn render_row(
    driver: DriverKind,
    cols: &[String],
    object: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut out = String::from("(");
    for (index, name) in cols.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match object.get(name) {
            Some(value) => out.push_str(&sql_literal(driver, value)),
            None => out.push_str("NULL"),
        }
    }
    out.push(')');
    out
}

fn sql_literal(driver: DriverKind, value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(true) => "TRUE".to_string(),
        serde_json::Value::Bool(false) => "FALSE".to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => quote_string(driver, text),
        nested => quote_string(driver, &nested.to_string()),
    }
}

fn quote_string(driver: DriverKind, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' if driver == DriverKind::Mysql => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

fn quote_ident(driver: DriverKind, ident: &str) -> String {
    match driver {
        DriverKind::Mysql => format!("`{}`", ident.replace('`', "``")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

pub(super) fn qualified_table(driver: DriverKind, schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        quote_ident(driver, schema),
        quote_ident(driver, table)
    )
}

/// 构造多行插入；Skip 和 Merge 使用数据库原生冲突跳过语法。
pub(super) fn build_insert_sql(
    driver: DriverKind,
    policy: ConflictPolicy,
    qualified: &str,
    cols: &[String],
    rows: &[String],
) -> String {
    let dedupe = matches!(policy, ConflictPolicy::Skip | ConflictPolicy::Merge);
    let verb = if dedupe && driver == DriverKind::Mysql {
        "INSERT IGNORE INTO"
    } else {
        "INSERT INTO"
    };
    let suffix = if dedupe && matches!(driver, DriverKind::Postgres | DriverKind::Sqlite) {
        "\nON CONFLICT DO NOTHING"
    } else {
        ""
    };
    let col_list = cols
        .iter()
        .map(|name| quote_ident(driver, name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{verb} {qualified} ({col_list}) VALUES\n{}{suffix}",
        rows.join(",\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_quoting_is_driver_specific() {
        assert_eq!(quote_ident(DriverKind::Mysql, "or`der"), "`or``der`");
        assert_eq!(
            quote_ident(DriverKind::Postgres, "or\"der"),
            "\"or\"\"der\""
        );
        assert_eq!(
            qualified_table(DriverKind::Mysql, "demo", "users"),
            "`demo`.`users`"
        );
    }

    #[test]
    fn literals_escape_per_driver() {
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::Value::Null),
            "NULL"
        );
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!(true)),
            "TRUE"
        );
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!(1.5)),
            "1.5"
        );
        assert_eq!(
            sql_literal(DriverKind::Mysql, &serde_json::json!("a'b\\c")),
            "'a''b\\\\c'"
        );
        assert_eq!(
            sql_literal(DriverKind::Postgres, &serde_json::json!("a'b\\c")),
            "'a''b\\c'"
        );
        assert_eq!(
            sql_literal(DriverKind::Postgres, &serde_json::json!({"k": 1})),
            "'{\"k\":1}'"
        );
    }

    #[test]
    fn insert_sql_applies_policy_per_engine() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let rows = vec!["(1, 'a')".to_string(), "(2, 'b')".to_string()];
        let mysql_skip = build_insert_sql(
            DriverKind::Mysql,
            ConflictPolicy::Skip,
            "`d`.`t`",
            &cols,
            &rows,
        );
        assert!(mysql_skip.starts_with("INSERT IGNORE INTO `d`.`t` (`id`, `name`) VALUES"));
        let pg_skip = build_insert_sql(
            DriverKind::Postgres,
            ConflictPolicy::Skip,
            "\"d\".\"t\"",
            &cols,
            &rows,
        );
        assert!(pg_skip.ends_with("ON CONFLICT DO NOTHING"));
        let sqlite_skip = build_insert_sql(
            DriverKind::Sqlite,
            ConflictPolicy::Skip,
            "\"main\".\"t\"",
            &cols,
            &rows,
        );
        assert!(sqlite_skip.ends_with("ON CONFLICT DO NOTHING"));
        let plain = build_insert_sql(
            DriverKind::Postgres,
            ConflictPolicy::Fail,
            "\"d\".\"t\"",
            &cols,
            &rows,
        );
        assert!(plain.starts_with("INSERT INTO"));
        assert!(!plain.contains("ON CONFLICT"));
    }

    #[test]
    fn render_row_serializes_in_column_order() {
        let row = serde_json::from_str::<serde_json::Value>(r#"{"name": "张三", "id": 7}"#)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let cols = vec!["id".to_string(), "name".to_string()];
        assert_eq!(render_row(DriverKind::Mysql, &cols, &row), "(7, '张三')");
    }
}
