use std::time::Duration;

use gpui::{AppContext as _, Context};
use ramag_domain::entities::MAX_SQL_QUERY_BYTES;

use super::QueryTab;
use crate::sql_completion::extract_tables_in_use_for_prefetch;

impl QueryTab {
    /// Schedules a delayed schema-column lookup so typing remains responsive.
    pub(in crate::views::query_tab) fn schedule_column_prefetch(&mut self, cx: &mut Context<Self>) {
        self.column_prefetch_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.prefetch_columns_for_used_tables(cx);
            });
        }));
    }

    /// Immediately refreshes column metadata for tables referenced by the editor SQL.
    pub(in crate::views::query_tab) fn prefetch_columns_now(&mut self, cx: &mut Context<Self>) {
        self.column_prefetch_task.take();
        self.prefetch_columns_for_used_tables(cx);
    }

    fn prefetch_columns_for_used_tables(&self, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let sql = self.editor.read(cx).value();
        if sql.len() > MAX_SQL_QUERY_BYTES {
            return;
        }
        let tables = extract_tables_in_use_for_prefetch(&sql);
        if tables.is_empty() {
            return;
        }

        let cache = self.schema_cache.clone();
        let resolved: Vec<(String, String)> = {
            let mut cache = cache.write();
            let mut resolved = Vec::new();
            for (maybe_schema, table) in tables {
                let schema = maybe_schema
                    .or_else(|| self.active_schema.clone())
                    .or_else(|| conn.database.clone())
                    .or_else(|| {
                        cache.tables.iter().find_map(|(schema, tables)| {
                            tables
                                .iter()
                                .any(|known| known.eq_ignore_ascii_case(&table))
                                .then(|| schema.clone())
                        })
                    });
                let Some(schema) = schema else {
                    continue;
                };
                let key = (schema, table);
                if !cache.begin_column_load(key.clone()) {
                    continue;
                }
                resolved.push(key);
            }
            resolved
        };
        if resolved.is_empty() {
            return;
        }

        let svc = self.service.clone();
        cx.background_spawn(async move {
            for (schema, table) in resolved {
                match svc.list_columns(&conn, &schema, &table).await {
                    Ok(cols) => {
                        let names: Vec<String> = cols.into_iter().map(|c| c.name).collect();
                        let mut cache = cache.write();
                        cache.finish_column_load(&(schema.clone(), table.clone()));
                        cache.cache_columns((schema, table), names);
                    }
                    Err(e) => {
                        cache
                            .write()
                            .finish_column_load(&(schema.clone(), table.clone()));
                        tracing::warn!(
                            operation = "sql_column_prefetch",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %schema,
                            table = %table,
                            error = %e,
                            "prefetch columns failed"
                        );
                    }
                }
            }
        })
        .detach();
    }
}
