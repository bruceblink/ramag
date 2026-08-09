use super::*;

impl TableTreePanel {
    /// 搜索按需补拉；库过多时由用户显式触发，避免请求雪崩。
    pub(super) fn ensure_search_coverage(&mut self, cx: &mut Context<Self>) {
        const AUTO_LOAD_MAX_SCHEMAS: usize = 50;
        if self.search.read(cx).value().trim().is_empty() {
            self.cancel_full_search(cx);
            let before = self.expanded.len();
            self.expanded
                .retain(|schema, _| self.open_schemas.contains(schema));
            self.table_columns
                .retain(|(schema, _), _| self.open_schemas.contains(schema));
            if self.expanded.len() != before {
                self.invalidate_tree_rows();
            }
            return;
        }
        let searchable_schemas = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .count();
        if searchable_schemas > AUTO_LOAD_MAX_SCHEMAS {
            return;
        }
        let missing: Vec<String> = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .map(|s| s.name.clone())
            .filter(|n| !self.expanded.contains_key(n))
            .collect();
        for name in missing {
            self.load_tables_for(name, cx);
        }
    }

    pub(super) fn toggle_schema(&mut self, schema_name: String, cx: &mut Context<Self>) {
        self.active_schema = Some(schema_name.clone());
        cx.emit(TreeEvent::SchemaActivated {
            schema: schema_name.clone(),
        });

        if !self.open_schemas.insert(schema_name.clone()) {
            self.open_schemas.remove(&schema_name);
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        let needs_load = self
            .expanded
            .get(&schema_name)
            .is_none_or(|entry| entry.error.is_some());
        if needs_load {
            self.load_tables_for(schema_name, cx);
        } else {
            self.invalidate_tree_rows();
            cx.notify();
        }
    }

    /// 顺序补拉限制并发；取消后丢弃过期结果。
    pub(super) fn load_all_tables_for_search(&mut self, cx: &mut Context<Self>) {
        if self.full_search.is_some() || self.search.read(cx).value().trim().is_empty() {
            return;
        }
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let missing: Vec<String> = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .map(|schema| schema.name.clone())
            .filter(|name| {
                self.expanded
                    .get(name)
                    .is_none_or(|entry| entry.error.is_some())
            })
            .collect();
        let new_entries = missing
            .iter()
            .filter(|schema| !self.expanded.contains_key(*schema))
            .count();
        if self.expanded.len().saturating_add(new_entries) > MAX_LOADED_SCHEMA_TABLES {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "全库搜索最多加载 {MAX_LOADED_SCHEMA_TABLES} 个 schema；请先选择具体 schema，或缩小数据库范围"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }
        if missing.is_empty() {
            return;
        }

        self.full_search_generation = self.full_search_generation.wrapping_add(1);
        let generation = self.full_search_generation;
        self.full_search = Some(FullSearchProgress {
            completed: 0,
            total: missing.len(),
            failed: 0,
            generation,
        });
        cx.notify();

        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            for schema in missing {
                let cache_generation = this
                    .update(cx, |this, _| {
                        this.schema_cache.write().begin_table_refresh(&schema)
                    })
                    .unwrap_or(0);
                if cache_generation == 0 {
                    break;
                }
                let result = service.list_tables(&conn, &schema).await;
                let should_continue = this
                    .update(cx, |this, cx| {
                        let is_current = this.connection.as_ref().map(|current| &current.id)
                            == Some(&conn.id)
                            && this
                                .full_search
                                .is_some_and(|progress| progress.generation == generation);
                        if !is_current {
                            this.schema_cache
                                .write()
                                .cancel_table_refresh(&schema, cache_generation);
                            return false;
                        }

                        let entry = this.expanded.entry(schema.clone()).or_default();
                        entry.loading = false;
                        match result {
                            Ok(tables) => {
                                let names = tables.iter().map(|table| table.name.clone()).collect();
                                let views = tables
                                    .iter()
                                    .filter(|table| table.is_view)
                                    .map(|table| table.name.clone())
                                    .collect();
                                this.schema_cache.write().finish_table_refresh(
                                    schema.clone(),
                                    cache_generation,
                                    names,
                                    views,
                                );
                                entry.tables = tables;
                                entry.error = None;
                            }
                            Err(err) => {
                                this.schema_cache
                                    .write()
                                    .cancel_table_refresh(&schema, cache_generation);
                                error!(
                                    operation = "sql_metadata_search_tables",
                                    connection_id = %conn.id,
                                    driver = ?conn.driver,
                                    schema = %schema,
                                    error = %err,
                                    "load full-search tables failed"
                                );
                                entry.error = Some(err.to_string());
                                if let Some(progress) = this.full_search.as_mut() {
                                    progress.failed += 1;
                                }
                            }
                        }

                        let mut done = false;
                        if let Some(progress) = this.full_search.as_mut() {
                            progress.completed += 1;
                            done = progress.completed == progress.total;
                        }
                        if done {
                            this.full_search = None;
                        }
                        this.invalidate_tree_rows();
                        cx.notify();
                        !done
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn cancel_full_search(&mut self, cx: &mut Context<Self>) {
        if self.full_search.take().is_some() {
            self.full_search_generation = self.full_search_generation.wrapping_add(1);
            cx.notify();
        }
    }

    pub(super) fn load_tables_for(&mut self, schema_name: String, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        if !self.expanded.contains_key(&schema_name) {
            while self.expanded.len() >= MAX_LOADED_SCHEMA_TABLES {
                let evict = self
                    .expanded
                    .keys()
                    .find(|schema| {
                        !self.open_schemas.contains(*schema)
                            && self.active_schema.as_ref() != Some(*schema)
                    })
                    .cloned();
                let Some(evict) = evict else {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::warning(format!(
                            "最多同时保留 {MAX_LOADED_SCHEMA_TABLES} 个 schema 的表列表，请先收起不再使用的 schema"
                        ))
                        .autohide(true),
                    );
                    cx.notify();
                    return;
                };
                self.expanded.remove(&evict);
                self.table_columns.retain(|(schema, _), _| schema != &evict);
            }
        }
        self.table_request_generation = self.table_request_generation.wrapping_add(1);
        let request_generation = self.table_request_generation;
        let entry = self.expanded.entry(schema_name.clone()).or_default();
        entry.loading = true;
        entry.error = None;
        entry.request_generation = request_generation;
        self.invalidate_tree_rows();
        cx.notify();

        let svc = self.service.clone();
        let schema_for_async = schema_name.clone();
        let metadata_generation = self.metadata_generation;
        let cache_generation = self
            .schema_cache
            .write()
            .begin_table_refresh(&schema_for_async);
        cx.spawn(async move |this, cx| {
            let result = svc.list_tables(&conn, &schema_for_async).await;
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conn.id)
                    && this
                        .expanded
                        .get(&schema_for_async)
                        .is_some_and(|entry| entry.request_generation == request_generation);
                if !is_current {
                    this.schema_cache
                        .write()
                        .cancel_table_refresh(&schema_for_async, cache_generation);
                    return;
                }
                match result {
                    Ok(tables) => {
                        let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
                        let view_set: std::collections::HashSet<String> = tables
                            .iter()
                            .filter(|t| t.is_view)
                            .map(|t| t.name.clone())
                            .collect();
                        this.schema_cache.write().finish_table_refresh(
                            schema_for_async.clone(),
                            cache_generation,
                            names,
                            view_set,
                        );
                        let Some(entry) = this.expanded.get_mut(&schema_for_async) else {
                            return;
                        };
                        entry.loading = false;
                        entry.tables = tables;
                        entry.error = None;
                    }
                    Err(e) => {
                        this.schema_cache
                            .write()
                            .cancel_table_refresh(&schema_for_async, cache_generation);
                        error!(
                            operation = "sql_metadata_tables",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %schema_for_async,
                            error = %e,
                            "load tables failed"
                        );
                        let Some(entry) = this.expanded.get_mut(&schema_for_async) else {
                            return;
                        };
                        entry.loading = false;
                        entry.error = Some(e.to_string());
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn handle_table_click(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some((schema.clone(), table.clone()));
        if self.active_schema.as_deref() != Some(schema.as_str()) {
            self.active_schema = Some(schema.clone());
            cx.emit(TreeEvent::SchemaActivated {
                schema: schema.clone(),
            });
        }
        cx.emit(TreeEvent::TableSelected { schema, table });
        cx.notify();
    }

    pub(super) fn handle_show_ddl(
        &mut self,
        schema: String,
        table: String,
        is_view: bool,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TreeEvent::ShowCreateTable {
            schema,
            table,
            is_view,
        });
    }

    pub(super) fn toggle_table_columns(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        let key = (schema.clone(), table.clone());
        if self.table_columns.remove(&key).is_some() {
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        if self.table_columns.len() >= MAX_EXPANDED_TABLE_COLUMNS {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "最多同时展开 {MAX_EXPANDED_TABLE_COLUMNS} 个表的列结构，请先收起不再查看的表"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }

        let Some(conn) = self.connection.clone() else {
            return;
        };

        self.column_request_generation = self.column_request_generation.wrapping_add(1);
        let request_generation = self.column_request_generation;
        self.table_columns.insert(
            key.clone(),
            TableColumns {
                loading: true,
                request_generation,
                ..Default::default()
            },
        );
        self.invalidate_tree_rows();
        cx.notify();

        let svc = self.service.clone();
        let schema_async = schema.clone();
        let table_async = table.clone();
        let metadata_generation = self.metadata_generation;
        cx.spawn(async move |this, cx| {
            // 索引或外键失败不阻塞列结构。
            let cols_fut = svc.list_columns(&conn, &schema_async, &table_async);
            let idx_fut = svc.list_indexes(&conn, &schema_async, &table_async);
            let fk_fut = svc.list_foreign_keys(&conn, &schema_async, &table_async);
            let (cols_res, idx_res, fk_res) = futures::join!(cols_fut, idx_fut, fk_fut);
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conn.id)
                    && this
                        .table_columns
                        .get(&key)
                        .is_some_and(|entry| entry.request_generation == request_generation);
                if !is_current {
                    return;
                }
                let Some(entry) = this.table_columns.get_mut(&key) else {
                    return;
                };
                entry.loading = false;
                match cols_res {
                    Ok(cols) => {
                        let col_names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
                        this.schema_cache
                            .write()
                            .cache_columns((schema_async.clone(), table_async.clone()), col_names);
                        entry.columns = cols;
                    }
                    Err(e) => {
                        error!(
                            operation = "sql_metadata_columns",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %schema_async,
                            table = %table_async,
                            error = %e,
                            "load columns failed"
                        );
                        entry.error = Some(e.to_string());
                    }
                }
                match idx_res {
                    Ok(ix) => entry.indexes = ix,
                    Err(e) => {
                        tracing::warn!(
                            operation = "sql_metadata_indexes",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %schema_async,
                            table = %table_async,
                            error = %e,
                            "load indexes failed"
                        );
                    }
                }
                match fk_res {
                    Ok(fk) => entry.foreign_keys = fk,
                    Err(e) => {
                        tracing::warn!(
                            operation = "sql_metadata_foreign_keys",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %schema_async,
                            table = %table_async,
                            error = %e,
                            "load foreign keys failed"
                        );
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }
}
