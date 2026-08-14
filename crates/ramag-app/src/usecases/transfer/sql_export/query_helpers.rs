use super::super::sql_catalog::{first_column_strings, parse_show_create, transfer_literal};
use super::PAGE_ROWS;
use crate::usecases::ConnectionService;
use ramag_domain::entities::{Column, ConnectionConfig, DriverKind, Query, Value, build_ddl_query};
use ramag_domain::error::{DomainError, Result};

pub(super) fn build_page_select(
    driver: DriverKind,
    qualified: &str,
    col_list: &str,
    pk: &[&Column],
    order_by: &str,
    last_key: &Option<Vec<Value>>,
    offset: u64,
) -> String {
    if pk.is_empty() {
        return format!("SELECT {col_list} FROM {qualified} LIMIT {PAGE_ROWS} OFFSET {offset};");
    }
    let where_clause = match last_key {
        None => String::new(),
        Some(values) => {
            let literals = values
                .iter()
                .map(|v| transfer_literal(v, driver))
                .collect::<Vec<_>>()
                .join(", ");
            if pk.len() == 1 {
                format!(
                    " WHERE {} > {literals}",
                    driver.quote_identifier(&pk[0].name)
                )
            } else {
                let cols = pk
                    .iter()
                    .map(|c| driver.quote_identifier(&c.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" WHERE ({cols}) > ({literals})")
            }
        }
    };
    format!(
        "SELECT {col_list} FROM {qualified}{where_clause} ORDER BY {order_by} LIMIT {PAGE_ROWS};"
    )
}

pub(super) async fn view_ddl(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    schema: &str,
    view: &str,
    driver: DriverKind,
) -> Result<String> {
    let sql = build_ddl_query(driver, schema, view, true);
    let result = svc.execute(config, &Query::new(sql)).await?;
    match driver {
        DriverKind::Mysql => {
            // 导入端可能没有原账号，因此移除 DEFINER。
            Ok(format!(
                "{};",
                strip_mysql_definer(&parse_show_create(&result)?)
            ))
        }
        _ => {
            let definition = first_column_strings(&result)
                .into_iter()
                .next()
                .ok_or_else(|| DomainError::QueryFailed(format!("视图 {view} 定义查询无结果")))?;
            let qualified = qualified_name(driver, schema, view);
            let body = definition.trim_end().trim_end_matches(';');
            Ok(format!("CREATE VIEW {qualified} AS\n{body};"))
        }
    }
}

pub(super) fn qualified_name(driver: DriverKind, schema: &str, name: &str) -> String {
    format!(
        "{}.{}",
        driver.quote_identifier(schema),
        driver.quote_identifier(name)
    )
}

/// 移除 MySQL DDL 中的 `DEFINER` 子句。
pub(super) fn strip_mysql_definer(ddl: &str) -> String {
    let Some(start) = ddl.find(" DEFINER=") else {
        return ddl.to_string();
    };
    let rest = &ddl[start + " DEFINER=".len()..];
    // 跳过用户名和主机名两个可能带反引号的段。
    let mut chars = rest.char_indices().peekable();
    let mut segments = 0;
    let mut in_quote = false;
    let mut end = rest.len();
    for (i, ch) in chars.by_ref() {
        match ch {
            '`' => in_quote = !in_quote,
            '@' if !in_quote && segments == 0 => segments = 1,
            ' ' if !in_quote => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    format!("{}{}", &ddl[..start], &rest[end..])
}

pub(super) async fn run_first_column(
    svc: &ConnectionService,
    config: &ConnectionConfig,
    sql: String,
) -> Result<Vec<String>> {
    let result = svc.execute(config, &Query::new(sql)).await?;
    Ok(first_column_strings(&result))
}
