use ramag_domain::entities::{ConnectionConfig, DriverKind};

use super::super::inline_text_preview;

pub(super) const MAX_VISIBLE_CATALOG_ITEMS: usize = 200;

pub(super) fn selected_source(
    sources: &[ConnectionConfig],
    selected: Option<usize>,
) -> Option<&ConnectionConfig> {
    selected.and_then(|index| sources.get(index))
}

pub(super) fn connection_label(connection: &ConnectionConfig) -> String {
    let environment = connection
        .environment
        .as_deref()
        .filter(|environment| !environment.trim().is_empty())
        .map(|environment| format!(" · {}", inline_text_preview(environment.trim(), 24)))
        .unwrap_or_default();
    let read_only = if connection.production {
        " · 只读"
    } else {
        ""
    };
    let scope = connection
        .database
        .as_deref()
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!(" · {}", inline_text_preview(scope, 40)))
        .unwrap_or_default();
    format!(
        "{}{}{} · {}:{}{}",
        inline_text_preview(&connection.name, 40),
        environment,
        read_only,
        inline_text_preview(&connection.host, 64),
        connection.port,
        scope
    )
}

pub(super) fn preferred_scope(connection: &ConnectionConfig, scopes: &[String]) -> Option<String> {
    if let Some(configured) = connection
        .database
        .as_deref()
        .filter(|configured| !configured.is_empty())
        && scopes.iter().any(|scope| scope == configured)
    {
        return Some(configured.to_string());
    }
    if connection.driver == DriverKind::Postgres && scopes.iter().any(|scope| scope == "public") {
        return Some("public".into());
    }
    scopes.first().cloned()
}

pub(super) fn visible_catalog_items(items: &[String], query: &str) -> (Vec<String>, usize) {
    let query = query.trim().to_lowercase();
    let matched = items
        .iter()
        .filter(|item| query.is_empty() || item.to_lowercase().contains(&query))
        .count();
    let visible = items
        .iter()
        .filter(|item| query.is_empty() || item.to_lowercase().contains(&query))
        .take(MAX_VISIBLE_CATALOG_ITEMS)
        .cloned()
        .collect();
    (visible, matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_connections_are_distinguished_by_endpoint_and_scope() {
        let mut first = ConnectionConfig::new_mongodb("业务库", "mongo-a.internal", 27017);
        first.database = Some("orders".into());
        let mut second = ConnectionConfig::new_mongodb("业务库", "mongo-b.internal", 27018);
        second.database = Some("archive".into());
        assert_ne!(connection_label(&first), connection_label(&second));
        assert!(connection_label(&first).contains("mongo-a.internal:27017"));
        assert!(connection_label(&second).contains("archive"));
    }

    #[test]
    fn connection_label_includes_environment_and_read_only_tags() {
        let mut connection = ConnectionConfig::new_mysql("业务库", "db.internal", 3306, "user");
        connection.environment = Some("prod".into());
        connection.production = true;

        assert_eq!(
            connection_label(&connection),
            "业务库 · prod · 只读 · db.internal:3306"
        );
    }

    #[test]
    fn source_connection_requires_an_explicit_valid_selection() {
        let sources = vec![ConnectionConfig::new_mongodb(
            "source",
            "mongo.internal",
            27017,
        )];
        assert!(selected_source(&sources, None).is_none());
        assert!(selected_source(&sources, Some(1)).is_none());
        assert_eq!(
            selected_source(&sources, Some(0)).map(|source| source.name.as_str()),
            Some("source")
        );
    }

    #[test]
    fn configured_scope_then_postgres_public_then_first_are_preferred() {
        let scopes = vec!["analytics".into(), "public".into()];
        let mut postgres = ConnectionConfig::new_mysql("pg", "localhost", 5432, "user");
        postgres.driver = DriverKind::Postgres;
        postgres.database = Some("analytics".into());
        assert_eq!(
            preferred_scope(&postgres, &scopes).as_deref(),
            Some("analytics")
        );
        postgres.database = Some("missing".into());
        assert_eq!(
            preferred_scope(&postgres, &scopes).as_deref(),
            Some("public")
        );
    }

    #[test]
    fn object_filter_reports_total_match_and_bounds_rendered_items() {
        let items: Vec<String> = (0..500).map(|index| format!("order_{index}")).collect();
        let (visible, matched) = visible_catalog_items(&items, "order_");
        assert_eq!(matched, 500);
        assert_eq!(visible.len(), MAX_VISIBLE_CATALOG_ITEMS);
        let (visible, matched) = visible_catalog_items(&items, "order_42");
        assert_eq!(matched, 11);
        assert_eq!(visible.len(), 11);
    }
}
