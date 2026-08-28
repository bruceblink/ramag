//! 表树收藏与最近访问状态。

use std::collections::HashSet;

use gpui::Context;
use ramag_domain::entities::{ConnectionId, Table};
use serde::{Deserialize, Serialize};

use super::TableTreePanel;

const TABLE_TREE_NAVIGATION_PREF: &str = "dbclient_table_navigation";
const MAX_NAVIGATION_PREF_BYTES: usize = 64 * 1024;
const MAX_TABLE_FAVORITES: usize = 512;
const MAX_RECENT_TABLES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) struct TableNavigationRef {
    pub(super) connection_id: ConnectionId,
    pub(super) schema: String,
    pub(super) table: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedTableNavigation {
    #[serde(default)]
    favorites: Vec<TableNavigationRef>,
    #[serde(default)]
    recent: Vec<TableNavigationRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum TableTreeFilter {
    #[default]
    All,
    Favorites,
    Recent,
}

impl TableTreeFilter {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::All => "全部对象",
            Self::Favorites => "仅收藏",
            Self::Recent => "最近访问",
        }
    }
}

fn parse_navigation_preference(json: &str) -> Result<PersistedTableNavigation, String> {
    if json.len() > MAX_NAVIGATION_PREF_BYTES {
        return Err(format!("表树导航偏好过大：{} bytes", json.len()));
    }
    let mut preference = serde_json::from_str::<PersistedTableNavigation>(json)
        .map_err(|error| format!("解析表树导航偏好失败：{error}"))?;
    preference
        .favorites
        .retain(|item| !item.schema.is_empty() && !item.table.is_empty());
    preference.favorites.truncate(MAX_TABLE_FAVORITES);
    preference
        .recent
        .retain(|item| !item.schema.is_empty() && !item.table.is_empty());
    let mut seen = HashSet::with_capacity(MAX_RECENT_TABLES);
    preference.recent.retain(|item| seen.insert(item.clone()));
    preference.recent.truncate(MAX_RECENT_TABLES);
    Ok(preference)
}

fn navigation_ref(
    connection_id: &ConnectionId,
    schema: impl Into<String>,
    table: impl Into<String>,
) -> TableNavigationRef {
    TableNavigationRef {
        connection_id: connection_id.clone(),
        schema: schema.into(),
        table: table.into(),
    }
}

pub(super) fn table_matches_filter(
    filter: TableTreeFilter,
    connection_id: Option<&ConnectionId>,
    schema: &str,
    table: &str,
    favorites: &HashSet<TableNavigationRef>,
    recent: &[TableNavigationRef],
) -> bool {
    if filter == TableTreeFilter::All {
        return true;
    }
    let Some(connection_id) = connection_id else {
        return false;
    };
    let reference = navigation_ref(connection_id, schema, table);
    match filter {
        TableTreeFilter::Favorites => favorites.contains(&reference),
        TableTreeFilter::Recent => recent.contains(&reference),
        TableTreeFilter::All => unreachable!("all filter is handled above"),
    }
}

pub(super) fn schema_has_navigation_item(
    filter: TableTreeFilter,
    connection_id: Option<&ConnectionId>,
    schema: &str,
    favorites: &HashSet<TableNavigationRef>,
    recent: &[TableNavigationRef],
) -> bool {
    if filter == TableTreeFilter::All {
        return true;
    }
    let Some(connection_id) = connection_id else {
        return false;
    };
    match filter {
        TableTreeFilter::Favorites => favorites
            .iter()
            .any(|item| item.connection_id == *connection_id && item.schema == schema),
        TableTreeFilter::Recent => recent
            .iter()
            .any(|item| item.connection_id == *connection_id && item.schema == schema),
        TableTreeFilter::All => unreachable!("all filter is handled above"),
    }
}

impl TableTreePanel {
    pub(super) fn load_navigation_state(&mut self, cx: &mut Context<Self>) {
        let Some(storage) = ramag_ui::theme::storage_from_cx(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let loaded = match storage.get_preference(TABLE_TREE_NAVIGATION_PREF).await {
                Ok(Some(json)) => match parse_navigation_preference(&json) {
                    Ok(preference) => Some(preference),
                    Err(error) => {
                        tracing::warn!(
                            operation = "dbclient_table_navigation_parse",
                            error,
                            "ignore invalid table navigation preference"
                        );
                        None
                    }
                },
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(
                        operation = "dbclient_table_navigation_load",
                        error = %error,
                        "load table navigation preference failed"
                    );
                    None
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(preference) = loaded {
                    this.navigation_favorites = preference.favorites.into_iter().collect();
                    this.recent_tables = preference.recent;
                    this.ensure_navigation_coverage(cx);
                    this.invalidate_tree_rows();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn current_table_ref(
        &self,
        schema: &str,
        table: &str,
    ) -> Option<TableNavigationRef> {
        self.connection
            .as_ref()
            .map(|connection| navigation_ref(&connection.id, schema, table))
    }

    pub(super) fn toggle_table_favorite(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        let Some(reference) = self.current_table_ref(&schema, &table) else {
            return;
        };
        if self.navigation_favorites.contains(&reference) {
            self.navigation_favorites.remove(&reference);
        } else if self.navigation_favorites.len() >= MAX_TABLE_FAVORITES {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "表收藏最多保留 {MAX_TABLE_FAVORITES} 项"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        } else {
            self.navigation_favorites.insert(reference);
        }
        self.persist_navigation_state(cx);
        self.invalidate_tree_rows();
        cx.notify();
    }

    pub(super) fn record_recent_table(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        let Some(reference) = self.current_table_ref(&schema, &table) else {
            return;
        };
        self.recent_tables.retain(|item| item != &reference);
        self.recent_tables.insert(0, reference);
        self.recent_tables.truncate(MAX_RECENT_TABLES);
        self.persist_navigation_state(cx);
    }

    pub(super) fn set_table_filter(&mut self, filter: TableTreeFilter, cx: &mut Context<Self>) {
        if self.table_filter == filter {
            return;
        }
        self.table_filter = filter;
        self.ensure_navigation_coverage(cx);
        self.invalidate_tree_rows();
        cx.notify();
    }

    pub(super) fn ensure_navigation_coverage(&mut self, cx: &mut Context<Self>) {
        if self.table_filter == TableTreeFilter::All {
            return;
        }
        let Some(connection_id) = self.connection.as_ref().map(|connection| &connection.id) else {
            return;
        };
        let references = match self.table_filter {
            TableTreeFilter::All => Vec::new(),
            TableTreeFilter::Favorites => self
                .navigation_favorites
                .iter()
                .filter(|item| &item.connection_id == connection_id)
                .map(|item| item.schema.clone())
                .collect(),
            TableTreeFilter::Recent => self
                .recent_tables
                .iter()
                .filter(|item| &item.connection_id == connection_id)
                .map(|item| item.schema.clone())
                .collect(),
        };
        let mut schemas = Vec::new();
        let mut seen = HashSet::new();
        for schema in references {
            if seen.insert(schema.clone())
                && self.schemas.iter().any(|item| item.name == schema)
                && !self.expanded.contains_key(&schema)
            {
                schemas.push(schema);
            }
        }
        for schema in schemas {
            self.load_tables_for(schema, cx);
        }
    }

    pub(super) fn persist_navigation_state(&self, cx: &mut Context<Self>) {
        let mut favorites: Vec<_> = self.navigation_favorites.iter().cloned().collect();
        favorites.sort_by(|left, right| {
            left.connection_id
                .to_string()
                .cmp(&right.connection_id.to_string())
                .then_with(|| left.schema.cmp(&right.schema))
                .then_with(|| left.table.cmp(&right.table))
        });
        let preference = PersistedTableNavigation {
            favorites,
            recent: self.recent_tables.clone(),
        };
        match serde_json::to_string(&preference) {
            Ok(json) => ramag_ui::preferences::persist_preference_latest(
                TABLE_TREE_NAVIGATION_PREF,
                json,
                cx,
            ),
            Err(error) => tracing::warn!(
                operation = "dbclient_table_navigation_serialize",
                error = %error,
                "serialize table navigation preference failed"
            ),
        }
    }

    pub(super) fn prune_navigation_schema(
        &mut self,
        schema: &str,
        tables: &[Table],
        cx: &mut Context<Self>,
    ) {
        let Some(connection_id) = self.connection.as_ref().map(|connection| &connection.id) else {
            return;
        };
        let table_names: HashSet<&str> = tables.iter().map(|table| table.name.as_str()).collect();
        let before_favorites = self.navigation_favorites.len();
        self.navigation_favorites.retain(|item| {
            &item.connection_id != connection_id
                || item.schema != schema
                || table_names.contains(item.table.as_str())
        });
        let before_recent = self.recent_tables.len();
        self.recent_tables.retain(|item| {
            &item.connection_id != connection_id
                || item.schema != schema
                || table_names.contains(item.table.as_str())
        });
        if before_favorites != self.navigation_favorites.len()
            || before_recent != self.recent_tables.len()
        {
            self.persist_navigation_state(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(connection_id: &ConnectionId, schema: &str, table: &str) -> TableNavigationRef {
        TableNavigationRef {
            connection_id: connection_id.clone(),
            schema: schema.into(),
            table: table.into(),
        }
    }

    #[test]
    fn parses_and_bounds_navigation_preference() {
        let connection_id = ConnectionId::new();
        let json = serde_json::json!({
            "favorites": [
                {"connection_id": connection_id, "schema": "public", "table": "users"},
                {"connection_id": connection_id, "schema": "", "table": "ignored"}
            ],
            "recent": [
                {"connection_id": connection_id, "schema": "public", "table": "users"},
                {"connection_id": connection_id, "schema": "public", "table": "orders"},
                {"connection_id": connection_id, "schema": "public", "table": "orders"}
            ]
        })
        .to_string();
        let preference = parse_navigation_preference(&json).expect("valid preference");
        assert_eq!(
            preference.favorites,
            vec![reference(&connection_id, "public", "users")]
        );
        assert_eq!(
            preference.recent,
            vec![
                reference(&connection_id, "public", "users"),
                reference(&connection_id, "public", "orders")
            ]
        );
    }

    #[test]
    fn filter_is_scoped_to_connection_and_mode() {
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();
        let favorites = HashSet::from([reference(&first_id, "public", "users")]);
        let recent = vec![reference(&first_id, "public", "orders")];

        assert!(table_matches_filter(
            TableTreeFilter::Favorites,
            Some(&first_id),
            "public",
            "users",
            &favorites,
            &recent
        ));
        assert!(!table_matches_filter(
            TableTreeFilter::Favorites,
            Some(&second_id),
            "public",
            "users",
            &favorites,
            &recent
        ));
        assert!(table_matches_filter(
            TableTreeFilter::Recent,
            Some(&first_id),
            "public",
            "orders",
            &favorites,
            &recent
        ));
        assert!(table_matches_filter(
            TableTreeFilter::All,
            Some(&second_id),
            "other",
            "table",
            &favorites,
            &recent
        ));
    }
}
