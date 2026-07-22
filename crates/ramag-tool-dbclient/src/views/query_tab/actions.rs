//! `impl QueryTab` 行为方法：运行 / 取消 / 格式化 / EXPLAIN / 错误高亮

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Window};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use ramag_domain::entities::{MAX_SQL_QUERY_BYTES, Query, QueryResult, Value};
use ramag_domain::error::DomainError;
use tracing::{error, info, warn};

use super::paging::{
    PAGE_SIZE, PageRequest, Pager, count_sql, page_sql, paging_base_sql, trim_page_sentinel,
};
use super::sql_utils::{
    detect_dangerous_statements, extract_statement_at_cursor, make_short_title,
    parse_mysql_error_line,
};
use super::{QueryTab, QueryTabEvent};
use crate::sql_completion::extract_tables_in_use_for_prefetch;
use crate::views::result_panel::{ResultPagination, ResultState, TotalRows};

impl QueryTab {
    /// 取出当前编辑器中的 SQL
    pub(super) fn current_sql(&self, cx: &gpui::App) -> gpui::SharedString {
        self.editor.read(cx).value()
    }

    /// 运行、解析或格式化前先拦住异常大的编辑器内容，避免复制和 CPU 峰值。
    fn checked_current_sql(
        &mut self,
        operation: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::SharedString> {
        let sql = self.current_sql(cx);
        if sql.len() <= MAX_SQL_QUERY_BYTES {
            return Some(sql);
        }
        self.result.update(cx, |result, cx| {
            result.set_state(
                ResultState::Error(format!(
                    "SQL 内容超过 {} MiB 安全上限，无法{operation}；请拆分脚本后重试",
                    MAX_SQL_QUERY_BYTES / 1024 / 1024
                )),
                cx,
            );
        });
        None
    }

    pub(super) fn handle_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = self.checked_current_sql("运行", cx) else {
            return;
        };
        let trimmed = sql.trim().to_string();
        // run = 用户主动执行，标题用原 SQL 派生，DDL 后刷新 cache
        let title_sql = trimmed.clone();
        self.submit_sql(trimmed, title_sql, true, window, cx);
    }

    /// 仅执行光标所在的那条 SQL（按 `;` 切分；避开字符串/注释/dollar-quoted 里的 `;`）
    pub(super) fn handle_run_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = self.checked_current_sql("运行", cx) else {
            return;
        };
        let cursor = self.editor.read(cx).cursor();
        let driver = self.connection.as_ref().map(|c| c.driver);
        let stmt = extract_statement_at_cursor(&sql, cursor, driver);
        let trimmed = stmt.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let title_sql = trimmed.clone();
        self.submit_sql(trimmed, title_sql, true, window, cx);
    }

    /// EXPLAIN 当前 SQL：把 SQL 包一层 `EXPLAIN ` 提交，结果展示在结果区
    /// 已经以 EXPLAIN 开头的 SQL 不重复加；末尾 `;` 自动 strip
    pub(crate) fn handle_explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = self.checked_current_sql("生成执行计划", cx) else {
            return;
        };
        let trimmed = sql.trim().trim_end_matches(';').trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let upper = trimmed.to_ascii_uppercase();
        let to_run = if upper.starts_with("EXPLAIN ") || upper == "EXPLAIN" {
            trimmed.clone()
        } else {
            format!("EXPLAIN {trimmed}")
        };
        // 标题用原 SQL（让 Tab 显示用户实际想看的语句，而不是 EXPLAIN xxx）
        // is_run=false：EXPLAIN 不会改 schema，跳过 DDL cache 刷新
        self.submit_sql(to_run, trimmed, false, window, cx);
    }

    /// run / explain 共用入口：高危语句（DELETE/UPDATE 无 WHERE、DROP、TRUNCATE）
    /// 先弹确认（显示连接 / 数据库 / 完整 SQL），确认后才进入执行；其余直接执行
    pub(super) fn submit_sql(
        &mut self,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running {
            return;
        }
        let Some(conn) = self.connection.clone() else {
            self.result.update(cx, |r, cx| {
                r.set_state(ResultState::Error("尚未选择连接".to_string()), cx);
            });
            return;
        };
        if sql_to_run.trim().is_empty() {
            self.result.update(cx, |r, cx| {
                r.set_state(ResultState::Error("SQL 为空".to_string()), cx);
            });
            return;
        }
        // EXPLAIN（is_run=false）只读不拦；生产只读连接由 driver 层拦截，无需在此确认
        let risks = if is_run && !conn.production {
            detect_dangerous_statements(&sql_to_run, conn.driver)
        } else {
            Vec::new()
        };
        if !risks.is_empty() {
            let entity = cx.entity();
            let confirmed_connection_id = conn.id.clone();
            let confirmed_schema = self.active_schema.clone();
            let confirmed_editor = self.current_sql(cx);
            let message =
                build_danger_prompt(&conn, self.active_schema.as_deref(), &risks, &sql_to_run);
            ramag_ui::open_confirm(
                "执行高危 SQL？",
                message,
                "仍要执行",
                true,
                move |_, app| {
                    entity.update(app, |this, cx| {
                        let context_changed = this
                            .connection
                            .as_ref()
                            .is_none_or(|current| current.id != confirmed_connection_id)
                            || this.active_schema != confirmed_schema
                            || this.current_sql(cx) != confirmed_editor;
                        if context_changed {
                            this.pending_notification = Some(
                                Notification::warning(
                                    "连接、数据库或 SQL 已变更，已取消执行；请重新确认",
                                )
                                .autohide(true),
                            );
                            cx.notify();
                            return;
                        }
                        this.submit_prepared(conn, sql_to_run, title_sql, is_run, cx);
                    });
                },
                window,
                cx,
            );
            return;
        }
        self.submit_prepared(conn, sql_to_run, title_sql, is_run, cx);
    }

    /// 结果工具条「导入」：对当前 pinned 表发起 JSONL 导入；
    /// 确认后上抛事件，由 session 路由到表树执行（进度条也显示在表树侧）
    pub(super) fn open_table_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((schema, table)) = self.pinned_target.clone() else {
            return;
        };
        let entity = cx.entity();
        ramag_ui::open_import_options_dialog(
            "导入 JSONL 到表",
            crate::views::table_tree::jsonl_import_description(&schema, &table),
            false,
            ("JSONL", &["jsonl", "json"]),
            move |policy, files, _, app| {
                entity.update(app, |_, cx| {
                    cx.emit(QueryTabEvent::TableImportRequested {
                        schema,
                        table,
                        policy,
                        files,
                    });
                });
            },
            window,
            cx,
        );
    }

    /// 确认后（或无需确认）提交执行
    fn submit_prepared(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        cx: &mut Context<Self>,
    ) {
        // 确认弹框期间可能已开始别的查询（如快捷键重复触发），再兜一次
        if self.running {
            return;
        }
        let pager = if is_run {
            paging_base_sql(&sql_to_run, conn.driver).map(|base_sql| Pager {
                base_sql,
                page: 0,
                has_more: false,
                page_size: PAGE_SIZE,
                total: TotalRows::Counting,
            })
        } else {
            None
        };
        let (effective_sql, page_request) = if let Some(pager) = pager.as_ref() {
            match page_sql(&pager.base_sql, pager.page_size, 0) {
                Ok(sql) => (
                    sql,
                    Some(PageRequest {
                        page: 0,
                        page_size: pager.page_size,
                    }),
                ),
                Err(message) => {
                    self.pager = None;
                    self.result.update(cx, |result, cx| {
                        result.set_state(ResultState::Error(message), cx);
                    });
                    return;
                }
            }
        } else {
            (sql_to_run, None)
        };
        self.pager = pager;
        // 分页首屏：后台并发精确计数（不写历史），算好回填“共 N 行”。翻页复用不重算。
        let count_base = self.pager.as_ref().map(|pager| pager.base_sql.clone());
        self.execute_query(
            conn.clone(),
            effective_sql,
            title_sql,
            is_run,
            page_request,
            cx,
        );
        if let Some(base_sql) = count_base {
            self.spawn_total_count(conn, base_sql, cx);
        }
    }

    /// 分页首屏后台精确计数：把原查询外包成 COUNT(*) 子查询执行（不写历史），
    /// 回填 Pager 与结果面板底栏。新查询通过 `count_seq` 令在途计数失效。
    fn spawn_total_count(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        base_sql: String,
        cx: &mut Context<Self>,
    ) {
        let counting_sql = match count_sql(&base_sql) {
            Ok(sql) => sql,
            Err(message) => {
                // 无法构造计数 SQL（极少见的超长查询）：底栏留空，不再尝试。
                warn!(error = %message, "build count sql failed");
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
        cx.spawn(async move |this, cx| {
            let mut query = Query::new(counting_sql);
            if let Some(schema) = active_schema {
                query = query.with_schema(schema);
            }
            let total = match svc.execute(&conn, &query).await {
                Ok(result) => parse_count_result(&result)
                    .map(TotalRows::Known)
                    .unwrap_or(TotalRows::Unavailable),
                Err(e) => {
                    warn!(error = %e, "count query failed");
                    TotalRows::Unavailable
                }
            };
            let _ = this.update(cx, |this, cx| {
                // 翻页不改 count_seq；仅当被新查询取代时丢弃本次计数。
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

    /// 请求相邻结果页；SQL 基线只保存在当前 QueryTab，不从可变编辑器重新读取。
    pub(super) fn handle_page(&mut self, requested_page: usize, cx: &mut Context<Self>) {
        if self.running {
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

    /// 执行核心：状态置忙 + 后台执行 + 回调落结果
    fn execute_query(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        page_request: Option<PageRequest>,
        cx: &mut Context<Self>,
    ) {
        self.running = true;
        self.run_seq = self.run_seq.wrapping_add(1);
        let request_seq = self.run_seq;
        self.query_start = Some(Instant::now());
        // 生产只读拦截（Forbidden）时需要恢复的原结果快照：不能让结果区停留在"执行中"
        let prev_state = self.result.read(cx).state().clone();
        self.result.update(cx, |r, cx| {
            r.set_state(ResultState::Running, cx);
        });
        cx.notify();

        // 后台 ticker：每 100ms notify 一次让耗时数字跳动
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
        // 结果的可编辑目标必须绑定到“发起查询时”的表，不能在回包时读取用户后来点击的表。
        let request_pinned_target = self.pinned_target.clone();
        let handle: ramag_domain::traits::CancelHandle =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        self.cancel_handle = Some(handle.clone());
        let task = cx.spawn(async move |this, cx| {
            let mut query = Query::new(sql_to_run);
            if let Some(s) = active_schema {
                query = query.with_schema(s);
            }
            let outcome = svc
                .execute_cancellable_with_history(&conn, &query, handle)
                .await;
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
                            rows = qr.rows.len(),
                            elapsed_ms = qr.elapsed_ms,
                            "query completed"
                        );
                        this.clear_sql_diagnostics(cx);
                        this.short_title = Some(make_short_title(&title_sql));
                        if is_run {
                            this.maybe_refresh_cache_after_ddl(&title_sql, cx);
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
                        // 表树单表数据：异步拉真实主键 / 唯一索引作为行定位键，
                        // 就绪前增删改保持禁用（绝不按列名猜键）
                        if let Some((schema, table)) = request_pinned_target {
                            this.fetch_row_identity(schema, table, cx);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "query failed");
                        let err_msg = e.to_string();
                        // 生产模式只读拦截：弹 toast 并恢复拦截前的结果快照
                        // （run 开始时已置 Running，不恢复会永久停在"执行中"）；
                        // 其余错误仍进结果区便于排查 / 复制
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

    /// 拉目标表元数据推导行定位键（真实主键，无主键回退全非空唯一索引），注入结果面板。
    /// 任一步失败仅记日志、键保持 None（行内修改 / 删除持续禁用，宁缺勿猜）
    fn fetch_row_identity(&self, schema: String, table: String, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let svc = self.service.clone();
        let result_handle = self.result.clone();
        cx.spawn(async move |_this, cx| {
            let identity = match svc.list_columns(&conn, &schema, &table).await {
                Ok(cols) => {
                    let has_pk = cols.iter().any(|c| c.is_primary_key);
                    // 有主键就不再多查一次索引
                    let indexes = if has_pk {
                        Vec::new()
                    } else {
                        match svc.list_indexes(&conn, &schema, &table).await {
                            Ok(idx) => idx,
                            Err(e) => {
                                tracing::warn!(error = %e, table = %table, "fetch indexes for row identity failed");
                                Vec::new()
                            }
                        }
                    };
                    crate::views::result_panel::derive_row_identity(&cols, &indexes)
                }
                Err(e) => {
                    tracing::warn!(error = %e, table = %table, "fetch columns for row identity failed");
                    None
                }
            };
            result_handle.update(cx, |r, cx| {
                r.set_row_identity_if_target(&schema, &table, identity, cx);
            });
        })
        .detach();
    }

    /// 检查 SQL 是否是 DDL（CREATE / DROP / ALTER / RENAME / TRUNCATE）
    /// 是的话后台拉默认 schema 的最新表名刷新 cache
    pub(super) fn maybe_refresh_cache_after_ddl(&self, sql: &str, cx: &mut Context<Self>) {
        let first = sql
            .split_whitespace()
            .next()
            .map(|w| w.to_ascii_uppercase())
            .unwrap_or_default();
        let is_ddl = matches!(
            first.as_str(),
            "CREATE" | "DROP" | "ALTER" | "RENAME" | "TRUNCATE"
        );
        if !is_ddl {
            return;
        }
        let Some(conn) = self.connection.clone() else {
            return;
        };
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
                        info!("schema cache refreshed after DDL");
                    } else {
                        tracing::debug!(
                            reason = "superseded_or_budget",
                            "DDL schema refresh discarded"
                        );
                    }
                }
                Err(e) => {
                    cache
                        .write()
                        .cancel_table_refresh(&schema, cache_generation);
                    error!(error = %e, "DDL schema refresh failed");
                }
            }
        })
        .detach();
    }

    /// 格式化当前编辑器的 SQL（替换原内容）
    pub(crate) fn handle_format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.formatting {
            self.pending_notification =
                Some(Notification::info("SQL 格式化正在进行").autohide(true));
            cx.notify();
            return;
        }
        let Some(sql) = self.checked_current_sql("格式化", cx) else {
            return;
        };
        if sql.trim().is_empty() {
            return;
        }
        self.formatting = true;
        cx.notify();
        let source_sql = sql.clone();
        cx.spawn_in(window, async move |this, async_cx| {
            let formatted = ramag_app::run_blocking(move || {
                let opts = sqlformat::FormatOptions {
                    indent: sqlformat::Indent::Spaces(2),
                    uppercase: Some(true),
                    lines_between_queries: 1,
                    ignore_case_convert: None,
                };
                Ok(sqlformat::format(
                    &sql,
                    &sqlformat::QueryParams::None,
                    &opts,
                ))
            })
            .await;
            let _ = this.update_in(async_cx, move |this, window, cx| {
                this.formatting = false;
                if this.current_sql(cx) != source_sql {
                    this.pending_notification = Some(
                        Notification::warning("SQL 已在格式化期间发生变化，未覆盖新内容")
                            .autohide(true),
                    );
                    cx.notify();
                    return;
                }
                match formatted {
                    Ok(formatted) if formatted.len() > MAX_SQL_QUERY_BYTES => {
                        this.pending_notification = Some(
                            Notification::error(format!(
                                "格式化结果超过 {} MiB 安全上限，已保留原 SQL",
                                MAX_SQL_QUERY_BYTES / 1024 / 1024
                            ))
                            .autohide(true),
                        );
                    }
                    Ok(formatted) if formatted != source_sql => {
                        this.clear_pager(cx);
                        this.editor.update(cx, |state, cx| {
                            state.set_value(formatted, window, cx);
                        });
                        this.prefetch_columns_now(cx);
                        cx.emit(super::QueryTabEvent::DraftChanged);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.pending_notification = Some(
                            Notification::error(format!("SQL 格式化失败：{error}")).autohide(true),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 报错后在编辑器对应行加红波浪线 + 错误消息（hover 显示）
    pub(super) fn highlight_sql_error(&mut self, err_msg: &str, cx: &mut Context<Self>) {
        let line_no = parse_mysql_error_line(err_msg);
        let msg_for_diag = err_msg.to_string();
        self.editor.update(cx, |state, cx| {
            if let Some(diag) = state.diagnostics_mut() {
                diag.clear();
                let line = line_no.unwrap_or(1).saturating_sub(1) as u32;
                let range = gpui_component::input::Position::new(line, 0)
                    ..gpui_component::input::Position::new(line, 9999);
                diag.push(
                    gpui_component::highlighter::Diagnostic::new(range, msg_for_diag)
                        .with_severity(gpui_component::highlighter::DiagnosticSeverity::Error),
                );
                cx.notify();
            }
        });
    }

    /// 清掉编辑器的错误高亮（运行成功 / 内容变化时）
    pub(super) fn clear_sql_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            if let Some(diag) = state.diagnostics_mut()
                && !diag.is_empty()
            {
                diag.clear();
                cx.notify();
            }
        });
    }

    /// 取消当前查询
    /// 1. drop Task 中断客户端 await（这步必然成功，反馈只承诺到这一层）
    /// 2. 若已拿到后端 thread id，detach 一个任务发 `KILL QUERY <id>`，
    ///    服务器确认 / 失败后再补一条准确的结果 toast
    pub(super) fn handle_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_task.take().is_none() {
            return;
        }
        self.run_seq = self.run_seq.wrapping_add(1);
        let cancel_target = self.cancel_handle.take().and_then(|h| {
            let tid = h.load(std::sync::atomic::Ordering::SeqCst);
            if tid > 0 { Some(tid) } else { None }
        });
        if let (Some(tid), Some(conn)) = (cancel_target, self.connection.clone()) {
            window.push_notification(
                Notification::info("已停止等待，正在请求服务器取消该查询…").autohide(true),
                cx,
            );
            let svc = self.service.clone();
            cx.spawn(async move |this, cx| {
                let outcome = svc.cancel_query(&conn, tid).await;
                let _ = this.update(cx, |this, cx| {
                    this.pending_notification = Some(match &outcome {
                        Ok(()) => Notification::success("服务器已确认取消查询").autohide(true),
                        Err(e) => Notification::warning(format!(
                            "服务器取消请求失败：{e}（客户端已停止等待，语句可能仍在服务器执行）"
                        ))
                        .autohide(true),
                    });
                    cx.notify();
                });
                match outcome {
                    Ok(()) => info!(thread_id = tid, "server query cancellation confirmed"),
                    Err(e) => tracing::warn!(error = %e, thread_id = tid, "server query cancellation failed"),
                }
            })
            .detach();
        } else {
            // 未拿到后端线程 id（尚在建连等早期阶段）：只能保证客户端不再等待
            window.push_notification(
                Notification::info("已停止等待；未获取到服务器线程，语句可能仍在服务器执行")
                    .autohide(true),
                cx,
            );
        }
        self.running = false;
        self.query_start = None;
        self.pager = None;
        self.result.update(cx, |r, cx| {
            r.set_state(ResultState::Empty, cx);
        });
        info!("client query wait cancelled");
        cx.notify();
    }

    /// 连续输入停顿后才扫描 SQL，避免每次按键都解析整段文本并发元数据请求。
    pub(super) fn schedule_column_prefetch(&mut self, cx: &mut Context<Self>) {
        self.column_prefetch_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.prefetch_columns_for_used_tables(cx);
            });
        }));
    }

    /// 程序整体替换 SQL 后立即预拉，并取消尚未触发的输入防抖任务。
    pub(super) fn prefetch_columns_now(&mut self, cx: &mut Context<Self>) {
        self.column_prefetch_task.take();
        self.prefetch_columns_for_used_tables(cx);
    }

    /// 扫描当前 SQL 找出 FROM / JOIN 涉及的表，对未在 cache 的表后台拉一次列结构
    /// schema 推断顺序：SQL 全限定 schema → active_schema → 连接默认 database → cache.tables 反查
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
                            error = %e, schema = %schema, table = %table,
                            "prefetch columns failed"
                        );
                    }
                }
            }
        })
        .detach();
    }
}

/// 从 COUNT(*) 结果取第一行第一列的非负整数总数；类型不符或为空则返回 None。
fn parse_count_result(result: &QueryResult) -> Option<u64> {
    match result.rows.first()?.values.first()? {
        Value::Int(n) => u64::try_from(*n).ok(),
        Value::Text(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// 高危 SQL 确认弹框文案：连接、目标数据库、命中的风险点与完整 SQL（超长截断展示）
fn build_danger_prompt(
    conn: &ramag_domain::entities::ConnectionConfig,
    active_schema: Option<&str>,
    risks: &[String],
    sql: &str,
) -> String {
    const SQL_PREVIEW_MAX: usize = 600;
    let database = active_schema
        .map(str::to_string)
        .or_else(|| conn.database.clone())
        .unwrap_or_else(|| "（未指定）".to_string());
    let risk_lines = risks
        .iter()
        .map(|r| format!("• {r}"))
        .collect::<Vec<_>>()
        .join("\n");
    let sql_trimmed = sql.trim();
    let sql_shown = sql_trimmed.char_indices().nth(SQL_PREVIEW_MAX).map_or_else(
        || sql_trimmed.to_string(),
        |(end, _)| {
            format!(
                "{}\n…（SQL 过长已截断展示，执行的是完整语句）",
                &sql_trimmed[..end]
            )
        },
    );
    format!(
        "连接：{}（{}:{}）\n数据库：{database}\n\n{risk_lines}\n\n完整 SQL：\n{sql_shown}",
        conn.name, conn.host, conn.port
    )
}
