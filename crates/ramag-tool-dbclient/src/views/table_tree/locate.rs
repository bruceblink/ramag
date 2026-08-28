use gpui::{Context, ScrollStrategy, Window};
use gpui_component::notification::Notification;

use super::{TableTreeFilter, TableTreePanel, row::TreeRow};

impl TableTreePanel {
    /// Makes a SQL table target visible without changing the query execution context.
    pub(crate) fn locate_table(
        &mut self,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connection.is_none() {
            self.pending_notification = Some(Notification::warning("尚未选择连接").autohide(true));
            cx.notify();
            return;
        }
        if !self.search_query.is_empty() {
            self.search.update(cx, |search, cx| {
                search.set_value(String::new(), window, cx);
            });
            self.search_query.clear();
        }
        self.table_filter = TableTreeFilter::All;
        self.selected = None;
        self.pending_navigation = Some((schema.clone(), table.clone()));
        self.active_schema = Some(schema);
        self.invalidate_tree_rows();
        self.start_pending_navigation(cx);
        cx.notify();
    }

    pub(super) fn start_pending_navigation(&mut self, cx: &mut Context<Self>) {
        let Some((target_schema, target_table)) = self.pending_navigation.clone() else {
            return;
        };
        let Some(schema) = self
            .schemas
            .iter()
            .find(|item| item.name == target_schema)
            .or_else(|| {
                self.schemas
                    .iter()
                    .find(|item| item.name.eq_ignore_ascii_case(&target_schema))
            })
            .map(|item| item.name.clone())
        else {
            if self.loading_schemas {
                return;
            }
            self.pending_navigation = None;
            self.pending_notification = Some(
                Notification::warning(format!("当前连接未找到 Schema：{target_schema}"))
                    .autohide(true),
            );
            cx.notify();
            return;
        };
        if schema != target_schema {
            self.pending_navigation = Some((schema.clone(), target_table));
        }
        self.active_schema = Some(schema.clone());

        self.open_schemas.insert(schema.clone());
        let loaded = self
            .expanded
            .get(&schema)
            .is_some_and(|entry| !entry.loading && entry.error.is_none());
        if loaded {
            self.reveal_pending_navigation(&schema, cx);
        } else {
            self.load_tables_for(schema, cx);
        }
    }

    pub(super) fn reveal_pending_navigation(&mut self, schema: &str, cx: &mut Context<Self>) {
        let Some((target_schema, target_table)) = self.pending_navigation.clone() else {
            return;
        };
        if target_schema != schema {
            return;
        }
        let Some(entry) = self.expanded.get(schema) else {
            return;
        };
        if entry.loading {
            return;
        }
        let actual_table = entry
            .tables
            .iter()
            .find(|table| table.name == target_table)
            .or_else(|| {
                entry
                    .tables
                    .iter()
                    .find(|table| table.name.eq_ignore_ascii_case(&target_table))
            })
            .map(|table| table.name.clone());
        let Some(actual_table) = actual_table else {
            self.pending_navigation = None;
            self.pending_notification = Some(
                Notification::warning(format!("当前连接未找到表：{schema}.{target_table}"))
                    .autohide(true),
            );
            cx.notify();
            return;
        };

        self.selected = Some((schema.to_string(), actual_table.clone()));
        self.record_recent_table(schema.to_string(), actual_table.clone(), cx);
        self.pending_navigation = None;
        self.invalidate_tree_rows();
        let view = self.tree_rows_view(&self.search_query);
        if let Some(index) = view.rows.iter().position(|row| {
            matches!(
                row,
                TreeRow::Table { key, .. }
                    if key.0 == schema && key.1 == actual_table
            )
        }) {
            self.uniform_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        self.pending_notification =
            Some(Notification::info(format!("已定位到 {schema}.{actual_table}")).autohide(true));
        cx.notify();
    }
}
