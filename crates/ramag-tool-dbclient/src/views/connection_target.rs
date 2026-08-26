//! SQL 结果和表结构对比共用的目标连接选择规则。

use ramag_domain::entities::{ConnectionConfig, DriverKind};

pub(super) const SQL_CONNECTION_SEPARATOR: &str = "::";
pub(super) const MAX_SQL_CONNECTION_HINTS: usize = 12;

pub(super) fn resolve_sql_connection(
    selector: &str,
    source: &ConnectionConfig,
    available: &[ConnectionConfig],
) -> Result<ConnectionConfig, &'static str> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("目标连接不能为空");
    }
    if !matches!(source.driver, DriverKind::Mysql | DriverKind::Postgres) {
        return Err("当前数据库暂不支持 SQL 连接对比");
    }

    let mut matches = Vec::new();
    if source.name.eq_ignore_ascii_case(selector)
        || source.id.to_string().eq_ignore_ascii_case(selector)
    {
        matches.push(source);
    }
    matches.extend(available.iter().filter(|connection| {
        connection.id != source.id
            && connection.driver == source.driver
            && (connection.name.eq_ignore_ascii_case(selector)
                || connection.id.to_string().eq_ignore_ascii_case(selector))
    }));
    match matches.as_slice() {
        [] => Err("找不到同类型的目标连接"),
        [connection] => Ok((*connection).clone()),
        _ => Err("目标连接名称不唯一，请改用连接 ID"),
    }
}

pub(super) fn sql_connection_hint(
    source: &ConnectionConfig,
    available: &[ConnectionConfig],
) -> String {
    let mut labels = vec![format!(
        "{}（当前连接，ID {}）",
        crate::views::inline_text_preview(&source.name, 64),
        source.id
    )];
    labels.extend(
        available
            .iter()
            .filter(|connection| connection.id != source.id && connection.driver == source.driver)
            .take(MAX_SQL_CONNECTION_HINTS)
            .map(|connection| {
                format!(
                    "{}（ID {}）",
                    crate::views::inline_text_preview(&connection.name, 64),
                    connection.id
                )
            }),
    );
    let other_count = available
        .iter()
        .filter(|connection| connection.id != source.id && connection.driver == source.driver)
        .count();
    let suffix = if other_count > MAX_SQL_CONNECTION_HINTS {
        " 等"
    } else {
        ""
    };
    format!("{}{}", labels.join("、"), suffix)
}

pub(super) fn has_sql_compare_target(
    source: &ConnectionConfig,
    available: &[ConnectionConfig],
) -> bool {
    available
        .iter()
        .any(|connection| connection.id != source.id && connection.driver == source.driver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_same_driver_connection_by_name_or_id() {
        let source = ConnectionConfig::new_mysql("source", "mysql-a", 3306, "root");
        let target = ConnectionConfig::new_mysql("target", "mysql-b", 3306, "root");
        let available = vec![source.clone(), target.clone()];

        assert_eq!(
            resolve_sql_connection(" target ", &source, &available),
            Ok(target.clone())
        );
        assert_eq!(
            resolve_sql_connection(&target.id.to_string(), &source, &available),
            Ok(target)
        );
    }

    #[test]
    fn rejects_duplicate_names_and_cross_driver_targets() {
        let source = ConnectionConfig::new_mysql("source", "mysql-a", 3306, "root");
        let duplicate = ConnectionConfig::new_mysql("target", "mysql-b", 3306, "root");
        let postgres = {
            let mut connection = ConnectionConfig::new_mysql("pg", "pg-a", 5432, "postgres");
            connection.driver = DriverKind::Postgres;
            connection
        };

        assert_eq!(
            resolve_sql_connection(
                "target",
                &source,
                &[source.clone(), duplicate.clone(), duplicate.clone()]
            ),
            Err("目标连接名称不唯一，请改用连接 ID")
        );
        assert_eq!(
            resolve_sql_connection("pg", &source, &[source.clone(), postgres]),
            Err("找不到同类型的目标连接")
        );
    }
}
