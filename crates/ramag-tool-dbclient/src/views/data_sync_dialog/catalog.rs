use std::collections::BTreeSet;

use ramag_domain::entities::{ConnectionConfig, DriverKind};

use super::super::inline_text_preview;

pub(super) const MAX_VISIBLE_CATALOG_ITEMS: usize = 200;
const MAX_PREFIX_SUGGESTIONS: usize = 200;

pub(super) fn selected_source(
    sources: &[ConnectionConfig],
    selected: Option<usize>,
) -> Option<&ConnectionConfig> {
    selected.and_then(|index| sources.get(index))
}

pub(super) fn connection_label(connection: &ConnectionConfig) -> String {
    let scope = connection
        .database
        .as_deref()
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!(" · {}", inline_text_preview(scope, 40)))
        .unwrap_or_default();
    format!(
        "{} · {}:{}{}",
        inline_text_preview(&connection.name, 40),
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

pub(super) fn prefix_suggestions(keys: &[String]) -> Vec<String> {
    let mut suggestions = BTreeSet::new();
    for key in keys {
        for (index, character) in key.char_indices() {
            if matches!(character, ':' | '/' | '.' | '-') {
                suggestions.insert(key[..index + character.len_utf8()].to_string());
                if suggestions.len() >= MAX_PREFIX_SUGGESTIONS {
                    return suggestions.into_iter().collect();
                }
            }
        }
    }
    suggestions.into_iter().collect()
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

    #[test]
    fn redis_prefixes_are_derived_from_real_keys() {
        let suggestions =
            prefix_suggestions(&["app:users:1".into(), "app:orders:2".into(), "plain".into()]);
        assert!(suggestions.contains(&"app:".to_string()));
        assert!(suggestions.contains(&"app:users:".to_string()));
        assert!(!suggestions.contains(&"plain".to_string()));
    }
}
