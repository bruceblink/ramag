use super::*;

impl CollectionTreePanel {
    /// 补齐搜索所需的集合列表。
    pub(super) fn ensure_search_coverage(&mut self, cx: &mut Context<Self>) {
        if self.search.read(cx).value().trim().is_empty() {
            self.cancel_search_load();
            let removed: Vec<String> = self
                .expanded
                .keys()
                .filter(|database| !self.open_databases.contains(*database))
                .cloned()
                .collect();
            for database in &removed {
                self.remove_expanded_entry(database);
            }
            if !removed.is_empty() {
                self.invalidate_tree_rows();
            }
            return;
        }
        if self.search_loading {
            return;
        }
        let searchable_databases = self
            .databases
            .iter()
            .filter(|database| self.show_system || !is_system_db(&database.name))
            .count();
        if searchable_databases > AUTO_LOAD_MAX_DATABASES {
            return;
        }
        let missing: Vec<String> = self
            .databases
            .iter()
            .filter(|database| self.show_system || !is_system_db(&database.name))
            .map(|d| d.name.clone())
            .filter(|name| {
                self.expanded
                    .get(name)
                    .is_none_or(|state| state.error.is_some())
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        let new_entries = missing
            .iter()
            .filter(|database| !self.expanded.contains_key(*database))
            .count();
        if self.expanded.len().saturating_add(new_entries) > MAX_LOADED_DATABASES {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "搜索最多加载 {MAX_LOADED_DATABASES} 个数据库；请先收起不再使用的数据库"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }

        self.search_load_generation = self.search_load_generation.wrapping_add(1);
        if self.search_load_generation == 0 {
            self.search_load_generation = 1;
        }
        let search_generation = self.search_load_generation;
        let metadata_generation = self.metadata_generation;
        let Some(conf) = self.connection.clone() else {
            return;
        };
        self.search_loading = true;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            for database in missing {
                let request_generation = this
                    .update(cx, |this, cx| {
                        let search_is_current = this.search_loading
                            && this.search_load_generation == search_generation
                            && this.metadata_generation == metadata_generation
                            && this.connection.as_ref().map(|current| &current.id)
                                == Some(&conf.id);
                        if !search_is_current {
                            return None;
                        }
                        if this
                            .expanded
                            .get(&database)
                            .is_some_and(|state| state.error.is_none())
                        {
                            return Some(0);
                        }
                        let request_generation = this.next_collection_request_generation();
                        let state = this.expanded.entry(database.clone()).or_default();
                        state.loading = true;
                        state.error = None;
                        state.request_generation = request_generation;
                        this.invalidate_tree_rows();
                        cx.notify();
                        Some(request_generation)
                    })
                    .ok()
                    .flatten();
                let Some(request_generation) = request_generation else {
                    return;
                };
                if request_generation == 0 {
                    continue;
                }

                let result = service.list_collections(&conf, &database).await;
                if let Err(error) = &result {
                    error!(
                        operation = "mongo_metadata_search_collections",
                        connection_id = %conf.id,
                        connection_name = %conf.name,
                        database = %database,
                        error = %error,
                        "load search collections failed"
                    );
                }
                let should_continue = this
                    .update(cx, |this, cx| {
                        let search_is_current = this.search_loading
                            && this.search_load_generation == search_generation
                            && this.metadata_generation == metadata_generation
                            && this.connection.as_ref().map(|current| &current.id)
                                == Some(&conf.id);
                        if !search_is_current {
                            return false;
                        }
                        if this
                            .expanded
                            .get(&database)
                            .is_none_or(|state| state.request_generation != request_generation)
                        {
                            return true;
                        }
                        match result {
                            Ok(collections) => {
                                info!(
                                    operation = "mongo_metadata_search_collections",
                                    connection_id = %conf.id,
                                    database = %database,
                                    count = collections.len(),
                                    "search collections loaded"
                                );
                                if let Err(message) =
                                    this.store_collections(&database, collections, false)
                                    && let Some(state) = this.expanded.get_mut(&database)
                                {
                                    tracing::warn!(
                                        operation = "mongo_metadata_search_collections_cache",
                                        connection_id = %conf.id,
                                        database = %database,
                                        error = %message,
                                        "store search collections failed"
                                    );
                                    state.loading = false;
                                    state.error = Some(message);
                                }
                            }
                            Err(error) => {
                                if let Some(state) = this.expanded.get_mut(&database) {
                                    state.loading = false;
                                    state.error = Some(error.to_string());
                                }
                            }
                        }
                        this.invalidate_tree_rows();
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !should_continue {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.search_load_generation == search_generation {
                    this.search_loading = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn cancel_search_load(&mut self) {
        if self.search_loading {
            self.search_load_generation = self.search_load_generation.wrapping_add(1);
            if self.search_load_generation == 0 {
                self.search_load_generation = 1;
            }
            self.search_loading = false;
            let search_only_loading: Vec<String> = self
                .expanded
                .iter()
                .filter(|(database, state)| {
                    state.loading && !self.open_databases.contains(*database)
                })
                .map(|(database, _)| database.clone())
                .collect();
            for database in search_only_loading {
                self.remove_expanded_entry(&database);
            }
            for (database, state) in &mut self.expanded {
                if state.loading && self.open_databases.contains(database) {
                    state.loading = false;
                    state.error = Some("加载已取消，请重试".to_string());
                }
            }
        }
    }

    pub(super) fn next_collection_request_generation(&mut self) -> u64 {
        self.collection_request_generation = self.collection_request_generation.wrapping_add(1);
        if self.collection_request_generation == 0 {
            self.collection_request_generation = 1;
        }
        self.collection_request_generation
    }

    pub(super) fn remove_expanded_entry(&mut self, database: &str) -> bool {
        let Some(removed) = self.expanded.remove(database) else {
            return false;
        };
        self.expanded_bytes = self.expanded_bytes.saturating_sub(removed.retained_bytes);
        true
    }

    pub(super) fn store_collections(
        &mut self,
        database: &str,
        collections: Vec<MongoCollection>,
        allow_evict: bool,
    ) -> Result<(), String> {
        self.store_collections_with_limit(
            database,
            collections,
            allow_evict,
            MAX_LOADED_COLLECTION_BYTES,
        )
    }

    pub(super) fn store_collections_with_limit(
        &mut self,
        database: &str,
        collections: Vec<MongoCollection>,
        allow_evict: bool,
        limit: usize,
    ) -> Result<(), String> {
        let retained_bytes =
            collection_list_retained_bytes(database, &collections, collections.capacity());
        if retained_bytes > limit {
            return Err(format!(
                "该数据库的集合元数据超过 {} MiB 缓存上限，请缩小数据库范围",
                limit / 1024 / 1024
            ));
        }
        let previous_bytes = self
            .expanded
            .get(database)
            .map_or(0, |state| state.retained_bytes);
        while allow_evict
            && prospective_collection_bytes(self.expanded_bytes, previous_bytes, retained_bytes)
                > limit
        {
            let evict = self
                .expanded
                .keys()
                .find(|cached| {
                    cached.as_str() != database
                        && !self.open_databases.contains(*cached)
                        && self.active_db.as_ref() != Some(*cached)
                })
                .cloned();
            let Some(evict) = evict else {
                break;
            };
            self.remove_expanded_entry(&evict);
        }
        if prospective_collection_bytes(self.expanded_bytes, previous_bytes, retained_bytes) > limit
        {
            return Err(format!(
                "已加载的集合元数据达到 {} MiB 上限，请清空搜索或收起不再使用的数据库后重试",
                limit / 1024 / 1024
            ));
        }
        let Some(state) = self.expanded.get_mut(database) else {
            return Err("集合列表请求已失效".to_string());
        };
        self.expanded_bytes = self
            .expanded_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(retained_bytes);
        state.collections = collections;
        state.retained_bytes = retained_bytes;
        state.loading = false;
        state.error = None;
        Ok(())
    }

    pub(super) fn toggle_database(&mut self, db: &str, cx: &mut Context<Self>) {
        self.active_db = Some(db.to_string());
        if !self.open_databases.insert(db.to_string()) {
            self.open_databases.remove(db);
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        let needs_load = self
            .expanded
            .get(db)
            .is_none_or(|state| state.error.is_some());
        if needs_load {
            self.load_collections(db.to_string(), cx);
        } else {
            self.invalidate_tree_rows();
            cx.notify();
        }
        cx.emit(TreeEvent::DatabaseActivated {
            database: db.to_string(),
        });
    }

    pub(super) fn load_collections(&mut self, db: String, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        if !self.expanded.contains_key(&db) {
            while self.expanded.len() >= MAX_LOADED_DATABASES {
                let evict = self
                    .expanded
                    .keys()
                    .find(|database| {
                        !self.open_databases.contains(*database)
                            && self.active_db.as_ref() != Some(*database)
                    })
                    .cloned();
                let Some(evict) = evict else {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::warning(format!(
                            "最多同时保留 {MAX_LOADED_DATABASES} 个数据库的集合列表，请先收起不再使用的数据库"
                        ))
                        .autohide(true),
                    );
                    cx.notify();
                    return;
                };
                self.remove_expanded_entry(&evict);
            }
        }
        let request_generation = self.next_collection_request_generation();
        let state = self.expanded.entry(db.clone()).or_default();
        state.loading = true;
        state.error = None;
        state.request_generation = request_generation;
        self.invalidate_tree_rows();
        cx.notify();
        let svc = self.service.clone();
        let db_for_async = db.clone();
        let metadata_generation = self.metadata_generation;
        cx.spawn(async move |this, cx| {
            let r = svc.list_collections(&conf, &db_for_async).await;
            if let Err(error) = &r {
                error!(
                    operation = "mongo_metadata_collections",
                    connection_id = %conf.id,
                    connection_name = %conf.name,
                    database = %db_for_async,
                    error = %error,
                    "load collections failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conf.id)
                    && this
                        .expanded
                        .get(&db_for_async)
                        .is_some_and(|state| state.request_generation == request_generation);
                if !is_current {
                    return;
                }
                match r {
                    Ok(cs) => {
                        info!(
                            operation = "mongo_metadata_collections",
                            connection_id = %conf.id,
                            database = %db_for_async,
                            count = cs.len(),
                            "collections loaded"
                        );
                        if let Err(message) = this.store_collections(&db_for_async, cs, true)
                            && let Some(state) = this.expanded.get_mut(&db_for_async)
                        {
                            tracing::warn!(
                                operation = "mongo_metadata_collections_cache",
                                connection_id = %conf.id,
                                database = %db_for_async,
                                error = %message,
                                "store collections failed"
                            );
                            state.loading = false;
                            state.error = Some(message);
                        }
                    }
                    Err(e) => {
                        if let Some(state) = this.expanded.get_mut(&db_for_async) {
                            state.loading = false;
                            state.error = Some(e.to_string());
                        }
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_collection(&mut self, db: String, coll: String, cx: &mut Context<Self>) {
        self.active_db = Some(db.clone());
        self.selected = Some((db.clone(), coll.clone()));
        cx.emit(TreeEvent::CollectionSelected {
            database: db,
            collection: coll,
        });
        cx.notify();
    }

    pub(super) fn select_database(&mut self, db: String, cx: &mut Context<Self>) {
        self.active_db = Some(db.clone());
        let opened = self.open_databases.insert(db.clone());
        let needs_load = self
            .expanded
            .get(&db)
            .is_none_or(|state| state.error.is_some());
        if needs_load {
            self.load_collections(db.clone(), cx);
        } else if opened {
            self.invalidate_tree_rows();
        }
        cx.emit(TreeEvent::DatabaseActivated { database: db });
        cx.notify();
    }

    pub(super) fn current_filter(&self, _cx: &gpui::App) -> String {
        self.search_query.clone()
    }
}
