use super::*;

impl QueryTab {
    pub(super) fn spawn_total_count(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        base_sql: String,
        cx: &mut Context<Self>,
    ) {
        let counting_sql = match count_sql(&base_sql) {
            Ok(sql) => sql,
            Err(message) => {
                warn!(
                    operation = "sql_count",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    sql_bytes = base_sql.len(),
                    error = %message,
                    "build count sql failed"
                );
                if let Some(pager) = self.pager.as_mut() {
                    pager.total = TotalRows::Unavailable;
                }
                return;
            }
        };
        self.count_seq = self.count_seq.wrapping_add(1);
        let count_seq = self.count_seq;
        let svc = self.service.clone();
        let result_handle = self.result.clone();
        let active_schema = self.active_schema.clone();
        let transaction_id = self.transaction.as_ref().map(|session| session.id.clone());
        cx.spawn(async move |this, cx| {
            let mut query = Query::new(counting_sql);
            if let Some(schema) = active_schema {
                query = query.with_schema(schema);
            }
            let outcome = match transaction_id.as_ref() {
                Some(transaction_id) => {
                    svc.execute_in_transaction_without_history(&conn, transaction_id, &query)
                        .await
                }
                None => svc.execute(&conn, &query).await,
            };
            let total = match outcome {
                Ok(result) => parse_count_result(&result)
                    .map(TotalRows::Known)
                    .unwrap_or(TotalRows::Unavailable),
                Err(e) => {
                    warn!(
                        operation = "sql_count",
                        connection_id = %conn.id,
                        driver = ?conn.driver,
                        schema = query.default_schema.as_deref().unwrap_or("-"),
                        sql_bytes = query.sql.len(),
                        error = %e,
                        "count query failed"
                    );
                    TotalRows::Unavailable
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.count_seq != count_seq {
                    return;
                }
                if let Some(pager) = this.pager.as_mut() {
                    pager.total = total;
                }
                result_handle.update(cx, |result, cx| {
                    result.set_pagination_total(total, cx);
                });
            });
        })
        .detach();
    }

    pub(crate) fn handle_page(&mut self, requested_page: usize, cx: &mut Context<Self>) {
        if self.running || self.transaction_busy {
            return;
        }
        let Some(conn) = self.connection.clone() else {
            self.clear_pager(cx);
            return;
        };
        let Some(pager) = self.pager.as_ref() else {
            return;
        };
        let is_previous = requested_page
            .checked_add(1)
            .is_some_and(|page| page == pager.page);
        let is_next = pager
            .page
            .checked_add(1)
            .is_some_and(|page| page == requested_page)
            && pager.has_more;
        if !is_previous && !is_next {
            return;
        }
        let base_sql = pager.base_sql.clone();
        let page_size = pager.page_size;
        let effective_sql = match page_sql(&base_sql, page_size, requested_page) {
            Ok(sql) => sql,
            Err(message) => {
                self.pending_notification =
                    Some(Notification::error(format!("加载分页失败：{message}")).autohide(true));
                self.clear_pager(cx);
                cx.notify();
                return;
            }
        };
        self.execute_query(
            conn,
            effective_sql,
            base_sql,
            false,
            Some(PageRequest {
                page: requested_page,
                page_size,
            }),
            cx,
        );
    }

    pub(super) fn execute_query(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        page_request: Option<PageRequest>,
        cx: &mut Context<Self>,
    ) {
        if self.transaction_busy {
            return;
        }
        self.running = true;
        self.run_seq = self.run_seq.wrapping_add(1);
        let request_seq = self.run_seq;
        self.query_start = Some(Instant::now());
        // 只读拦截时恢复原结果，避免一直显示执行中。
        let prev_state = self.result.read(cx).state().clone();
        self.result.update(cx, |r, cx| {
            r.set_state(ResultState::Running, cx);
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let still_running = this
                    .update(cx, |this, cx| {
                        if this.running && this.run_seq == request_seq {
                            cx.notify();
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !still_running {
                    break;
                }
            }
        })
        .detach();

        let svc = self.service.clone();
        let result_handle = self.result.clone();
        let active_schema = self.active_schema.clone();
        let transaction_id = self.transaction.as_ref().map(|session| session.id.clone());
        let transaction_writes = transaction_id.is_some()
            && ramag_infra_sql_shared::sql::is_write_statement(&sql_to_run);
        // 结果的可编辑目标必须绑定到“发起查询时”的表，不能在回包时读取用户后来点击的表。
        let request_pinned_target = self.pinned_target.clone();
        let cancel_handle = transaction_id.is_none().then(|| {
            Arc::new(std::sync::atomic::AtomicU64::new(0)) as ramag_domain::traits::CancelHandle
        });
        self.cancel_handle = cancel_handle.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut query = Query::new(sql_to_run);
            if let Some(s) = active_schema {
                query = query.with_schema(s);
            }
            let outcome = match transaction_id.as_ref() {
                Some(transaction_id) => {
                    svc.execute_in_transaction(&conn, transaction_id, &query)
                        .await
                }
                None => {
                    let Some(cancel_handle) = cancel_handle else {
                        return;
                    };
                    svc.execute_cancellable_with_history(&conn, &query, cancel_handle)
                        .await
                }
            };
            if let Err(error) = &outcome {
                error!(
                    operation = "sql_query",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    schema = query.default_schema.as_deref().unwrap_or("-"),
                    sql_bytes = query.sql.len(),
                    error = %error,
                    "query failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                if this.run_seq != request_seq {
                    return;
                }
                this.running = false;
                this.current_task = None;
                this.cancel_handle = None;
                this.query_start = None;
                match outcome {
                    Ok(mut qr) => {
                        let pagination = if let Some(request) = page_request {
                            let has_more = trim_page_sentinel(&mut qr, request.page_size);
                            this.pager.as_mut().and_then(|pager| {
                                (pager.page_size == request.page_size).then(|| {
                                    pager.page = request.page;
                                    pager.has_more = has_more;
                                    ResultPagination {
                                        page: request.page,
                                        page_size: request.page_size,
                                        has_more,
                                        total: pager.total,
                                    }
                                })
                            })
                        } else {
                            None
                        };
                        info!(
                            operation = "sql_query",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = query.default_schema.as_deref().unwrap_or("-"),
                            sql_bytes = query.sql.len(),
                            rows = qr.rows.len(),
                            elapsed_ms = qr.elapsed_ms,
                            "query completed"
                        );
                        this.clear_sql_diagnostics(cx);
                        this.short_title = Some(make_short_title(&title_sql));
                        if is_run && !transaction_writes {
                            this.maybe_refresh_cache_after_ddl(&title_sql, cx);
                        }
                        if transaction_writes {
                            this.mark_transaction_dirty(cx);
                        }
                        let target_for_result = request_pinned_target
                            .as_ref()
                            .map(|(s, t)| (Some(s.clone()), t.clone()));
                        result_handle.update(cx, |r, cx| {
                            r.set_source_sql(Some(title_sql.clone()));
                            r.set_pinned_target(target_for_result);
                            r.set_state(ResultState::Ok(Arc::new(qr)), cx);
                            r.set_pagination(pagination, cx);
                        });
                        // 就绪前禁用行写操作，绝不按列名猜定位键。
                        if let Some((schema, table)) = request_pinned_target {
                            this.fetch_row_identity(schema, table, cx);
                        }
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        // 只读拦截恢复执行前结果，其余错误显示在结果区。
                        if matches!(e, DomainError::Forbidden(_)) {
                            this.pending_notification =
                                Some(Notification::warning(err_msg).autohide(true));
                            result_handle.update(cx, |r, cx| {
                                r.restore_state(prev_state, cx);
                            });
                        } else {
                            this.highlight_sql_error(&err_msg, cx);
                            result_handle.update(cx, |r, cx| {
                                r.set_state(ResultState::Error(err_msg), cx);
                            });
                        }
                    }
                }
                cx.notify();
            });
        });
        self.current_task = Some(task);
    }

    /// 仅使用主键或全非空唯一索引定位行。
    pub(super) fn fetch_row_identity(&self, schema: String, table: String, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let svc = self.service.clone();
        let result_handle = self.result.clone();
        cx.spawn(async move |_this, cx| {
            let identity = match svc.list_columns(&conn, &schema, &table).await {
                Ok(cols) => {
                    let has_pk = cols.iter().any(|c| c.is_primary_key);
                    let indexes = if has_pk {
                        Vec::new()
                    } else {
                        match svc.list_indexes(&conn, &schema, &table).await {
                            Ok(idx) => idx,
                            Err(e) => {
                                tracing::warn!(
                                    operation = "sql_row_identity",
                                    connection_id = %conn.id,
                                    driver = ?conn.driver,
                                    schema = %schema,
                                    table = %table,
                                    error = %e,
                                    "fetch indexes for row identity failed"
                                );
                                Vec::new()
                            }
                        }
                    };
                    crate::views::result_panel::derive_row_identity(&cols, &indexes)
                }
                Err(e) => {
                    tracing::warn!(
                        operation = "sql_row_identity",
                        connection_id = %conn.id,
                        driver = ?conn.driver,
                        schema = %schema,
                        table = %table,
                        error = %e,
                        "fetch columns for row identity failed"
                    );
                    None
                }
            };
            result_handle.update(cx, |r, cx| {
                r.set_row_identity_if_target(&schema, &table, identity, cx);
            });
        })
        .detach();
    }

    /// DDL 后刷新表缓存。
    pub(super) fn maybe_refresh_cache_after_ddl(&self, sql: &str, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let body = strip_leading_comments(sql, conn.driver);
        let first = ramag_infra_sql_shared::sql::first_keyword(body).unwrap_or_default();
        let is_ddl = matches!(
            first.as_str(),
            "CREATE" | "DROP" | "ALTER" | "RENAME" | "TRUNCATE"
        );
        if !is_ddl {
            return;
        }
        let Some(schema) = self
            .active_schema
            .clone()
            .or_else(|| conn.database.clone())
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let svc = self.service.clone();
        let cache = self.schema_cache.clone();
        let log_schema = schema.clone();
        let cache_generation = {
            let mut cache = cache.write();
            cache.invalidate_schema(&schema);
            cache.begin_table_refresh(&schema)
        };
        cx.background_spawn(async move {
            match svc.list_tables(&conn, &schema).await {
                Ok(tables) => {
                    let names = tables.iter().map(|table| table.name.clone()).collect();
                    let views = tables
                        .into_iter()
                        .filter(|table| table.is_view)
                        .map(|table| table.name)
                        .collect();
                    let refreshed =
                        cache
                            .write()
                            .finish_table_refresh(schema, cache_generation, names, views);
                    if refreshed {
                        info!(
                            operation = "sql_ddl_cache_refresh",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %log_schema,
                            "schema cache refreshed after DDL"
                        );
                    } else {
                        tracing::debug!(
                            operation = "sql_ddl_cache_refresh",
                            connection_id = %conn.id,
                            driver = ?conn.driver,
                            schema = %log_schema,
                            reason = "superseded_or_budget",
                            "DDL schema refresh discarded"
                        );
                    }
                }
                Err(e) => {
                    cache
                        .write()
                        .cancel_table_refresh(&schema, cache_generation);
                    error!(
                        operation = "sql_ddl_cache_refresh",
                        connection_id = %conn.id,
                        driver = ?conn.driver,
                        schema = %schema,
                        error = %e,
                        "refresh schema after DDL failed"
                    );
                }
            }
        })
        .detach();
    }
}
