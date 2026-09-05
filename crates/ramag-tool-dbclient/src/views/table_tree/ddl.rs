//! 表结构 SQL 构造与结果处理。

use std::collections::HashMap;

use ramag_domain::entities::{ConnectionConfig, DriverKind, Query, Value};

use super::TableColumns;

pub(super) fn success_message(message: &str, elapsed_ms: u64) -> String {
    if elapsed_ms < 1_000 {
        return message.to_string();
    }
    format!(
        "{message}（数据库耗时 {:.1} 秒）",
        elapsed_ms as f64 / 1_000.0
    )
}

pub(super) fn clear_invalidated_table_state(
    selected: &mut Option<(String, String)>,
    table_columns: &mut HashMap<(String, String), TableColumns>,
    schema: &str,
    table: &str,
) {
    if selected
        .as_ref()
        .is_some_and(|(selected_schema, selected_table)| {
            selected_schema == schema && selected_table == table
        })
    {
        *selected = None;
    }
    table_columns.remove(&(schema.to_string(), table.to_string()));
}

pub(super) async fn load_table_ddl(
    service: &ramag_app::ConnectionService,
    connection: &ConnectionConfig,
    schema: &str,
    table: &str,
) -> anyhow::Result<String> {
    let sql = ramag_domain::entities::build_ddl_query(connection.driver, schema, table, false);
    let result = service.execute(connection, &Query::new(sql)).await?;
    result
        .rows
        .first()
        .and_then(|row| row.values.iter().rev().find_map(value_as_ddl))
        .ok_or_else(|| anyhow::anyhow!("数据库未返回 {schema}.{table} 的建表语句"))
}

fn value_as_ddl(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Json(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn ddl_truncate_table(driver: DriverKind, schema: &str, table: &str) -> String {
    if driver == DriverKind::Sqlite {
        return format!(
            "DELETE FROM {}.{}",
            driver.quote_identifier(schema),
            driver.quote_identifier(table)
        );
    }
    format!(
        "TRUNCATE TABLE {}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(table)
    )
}

pub(super) fn ddl_drop_table(
    driver: DriverKind,
    schema: &str,
    table: &str,
    is_view: bool,
) -> String {
    let kind = if is_view { "VIEW" } else { "TABLE" };
    format!(
        "DROP {kind} {}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(table)
    )
}

pub(super) fn ddl_rename_table(
    driver: DriverKind,
    schema: &str,
    old: &str,
    new: &str,
    is_view: bool,
) -> String {
    let schema = driver.quote_identifier(schema);
    let old = driver.quote_identifier(old);
    let new = driver.quote_identifier(new);
    match driver {
        DriverKind::Postgres => {
            let kind = if is_view { "VIEW" } else { "TABLE" };
            format!("ALTER {kind} {schema}.{old} RENAME TO {new}")
        }
        DriverKind::Sqlite => format!("ALTER TABLE {schema}.{old} RENAME TO {new}"),
        _ => format!("RENAME TABLE {schema}.{old} TO {schema}.{new}"),
    }
}

pub(super) fn ddl_drop_schema(driver: DriverKind, schema: &str) -> Result<String, String> {
    let schema = driver.quote_identifier(schema);
    match driver {
        DriverKind::Postgres => Ok(format!("DROP SCHEMA {schema} CASCADE")),
        DriverKind::Sqlite => Err("SQLite 不支持删除 schema；请单独删除表或视图".into()),
        _ => Ok(format!("DROP DATABASE {schema}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_quotes_by_dialect() {
        assert_eq!(
            ddl_truncate_table(DriverKind::Mysql, "shop", "order"),
            "TRUNCATE TABLE `shop`.`order`"
        );
        assert_eq!(
            ddl_truncate_table(DriverKind::Postgres, "public", "order"),
            "TRUNCATE TABLE \"public\".\"order\""
        );
        assert_eq!(
            ddl_truncate_table(DriverKind::Sqlite, "main", "order"),
            "DELETE FROM \"main\".\"order\""
        );
    }

    #[test]
    fn drop_table_and_view() {
        assert_eq!(
            ddl_drop_table(DriverKind::Mysql, "shop", "t1", false),
            "DROP TABLE `shop`.`t1`"
        );
        assert_eq!(
            ddl_drop_table(DriverKind::Postgres, "public", "v1", true),
            "DROP VIEW \"public\".\"v1\""
        );
    }

    #[test]
    fn drop_schema_dialect_split() {
        assert_eq!(
            ddl_drop_schema(DriverKind::Mysql, "shop").unwrap(),
            "DROP DATABASE `shop`"
        );
        assert_eq!(
            ddl_drop_schema(DriverKind::Postgres, "app").unwrap(),
            "DROP SCHEMA \"app\" CASCADE"
        );
        assert!(ddl_drop_schema(DriverKind::Sqlite, "main").is_err());
    }

    #[test]
    fn identifier_escaping() {
        assert_eq!(
            ddl_drop_schema(DriverKind::Mysql, "a`b").unwrap(),
            "DROP DATABASE `a``b`"
        );
    }

    #[test]
    fn rename_table_dialect_split() {
        assert_eq!(
            ddl_rename_table(DriverKind::Mysql, "shop", "t1", "t2", false),
            "RENAME TABLE `shop`.`t1` TO `shop`.`t2`"
        );
        assert_eq!(
            ddl_rename_table(DriverKind::Postgres, "public", "t1", "t2", false),
            "ALTER TABLE \"public\".\"t1\" RENAME TO \"t2\""
        );
        assert_eq!(
            ddl_rename_table(DriverKind::Postgres, "public", "v1", "v2", true),
            "ALTER VIEW \"public\".\"v1\" RENAME TO \"v2\""
        );
        assert_eq!(
            ddl_rename_table(DriverKind::Sqlite, "main", "t1", "t2", false),
            "ALTER TABLE \"main\".\"t1\" RENAME TO \"t2\""
        );
    }

    #[test]
    fn successful_table_ddl_clears_only_invalidated_local_state() {
        let mut selected = Some(("public".to_string(), "users".to_string()));
        let mut columns = HashMap::from([
            (
                ("public".to_string(), "users".to_string()),
                TableColumns::default(),
            ),
            (
                ("public".to_string(), "posts".to_string()),
                TableColumns::default(),
            ),
        ]);

        clear_invalidated_table_state(&mut selected, &mut columns, "public", "users");

        assert!(selected.is_none());
        assert!(!columns.contains_key(&("public".into(), "users".into())));
        assert!(columns.contains_key(&("public".into(), "posts".into())));
    }

    #[test]
    fn slow_ddl_success_message_reports_database_time() {
        assert_eq!(success_message("已修改表", 999), "已修改表");
        assert_eq!(
            success_message("已修改表", 9_050),
            "已修改表（数据库耗时 9.1 秒）"
        );
    }
}
