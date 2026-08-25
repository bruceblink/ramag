mod execution;
mod prefetch;
mod sorting;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, Window};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use ramag_domain::entities::{MAX_SQL_QUERY_BYTES, Query, QueryResult, Value};
use ramag_domain::error::DomainError;
use tracing::{error, info, warn};

use super::paging::{
    PageRequest, Pager, count_sql, filter_sql, page_sql, paging_base_sql, trim_page_sentinel,
};
use super::sql_utils::{
    detect_dangerous_statements, extract_statement_at_cursor, make_short_title,
    parse_mysql_error_line, strip_leading_comments,
};
use super::{QueryResultTarget, QueryTab, QueryTabEvent};
use crate::views::result_panel::{ResultPagination, ResultState, TotalRows};

impl QueryTab {
    pub(super) fn current_sql(&self, cx: &gpui::App) -> gpui::SharedString {
        self.editor.read(cx).value()
    }

    fn checked_current_sql(
        &mut self,
        operation: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::SharedString> {
        let sql = self.current_sql(cx);
        if sql.len() <= MAX_SQL_QUERY_BYTES {
            return Some(sql);
        }
        self.active_result().update(cx, |result, cx| {
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
        if self.running || self.transaction_busy {
            return;
        }
        self.show_plan = false;
        self.set_result_active(self.result_active, cx);
        let Some(sql) = self.checked_current_sql("运行", cx) else {
            return;
        };
        let trimmed = sql.trim().to_string();
        let title_sql = trimmed.clone();
        self.submit_sql(
            trimmed,
            title_sql,
            true,
            self.result.clone(),
            None,
            window,
            cx,
        );
    }

    pub(super) fn handle_run_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.transaction_busy {
            return;
        }
        self.show_plan = false;
        self.set_result_active(self.result_active, cx);
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
        self.submit_sql(
            trimmed,
            title_sql,
            true,
            self.result.clone(),
            None,
            window,
            cx,
        );
    }

    pub(crate) fn handle_explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.transaction_busy {
            return;
        }
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
        self.plan_seq = self.plan_seq.wrapping_add(1);
        self.show_plan = true;
        self.set_result_active(self.result_active, cx);
        self.submit_sql(
            to_run,
            trimmed,
            false,
            self.plan_result.clone(),
            Some(self.plan_seq),
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn submit_sql(
        &mut self,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        result_handle: Entity<crate::views::result_panel::ResultPanel>,
        plan_seq: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.transaction_busy {
            return;
        }
        let result_handle = self.active_result();
        let Some(conn) = self.connection.clone() else {
            result_handle.update(cx, |r, cx| {
                r.set_state(ResultState::Error("尚未选择连接".to_string()), cx);
            });
            return;
        };
        if sql_to_run.trim().is_empty() {
            result_handle.update(cx, |r, cx| {
                r.set_state(ResultState::Error("SQL 为空".to_string()), cx);
            });
            return;
        }
        let risks = if (is_run || plan_seq.is_some()) && !conn.production {
            detect_dangerous_statements(&sql_to_run, conn.driver)
        } else {
            Vec::new()
        };
        if !risks.is_empty() {
            let entity = cx.entity();
            let result_handle_for_confirm = result_handle.clone();
            let confirmed_connection_id = conn.id.clone();
            let confirmed_schema = self.active_schema.clone();
            let confirmed_editor = self.current_sql(cx);
            let result_handle_for_confirm = result_handle.clone();
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
                            || this.current_sql(cx) != confirmed_editor
                            || plan_seq.is_some_and(|seq| this.plan_seq != seq);
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
                        this.submit_prepared(
                            conn,
                            sql_to_run,
                            title_sql,
                            is_run,
                            result_handle_for_confirm,
                            plan_seq,
                            cx,
                        );
                    });
                },
                window,
                cx,
            );
            return;
        }
        self.submit_prepared(
            conn,
            sql_to_run,
            title_sql,
            is_run,
            result_handle,
            plan_seq,
            cx,
        );
    }

    pub(super) fn open_table_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_plan {
            return;
        }
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

    #[allow(clippy::too_many_arguments)]
    fn submit_prepared(
        &mut self,
        conn: ramag_domain::entities::ConnectionConfig,
        sql_to_run: String,
        title_sql: String,
        is_run: bool,
        result_handle: Entity<crate::views::result_panel::ResultPanel>,
        plan_seq: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.transaction_busy {
            return;
        }
        let pager = if is_run {
            paging_base_sql(&sql_to_run, conn.driver).map(|base_sql| Pager {
                base_sql: base_sql.clone(),
                sort_base_sql: base_sql,
                page: 0,
                has_more: false,
                page_size: self.page_size,
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
        // An explain request has its own result panel and must not discard the data result's
        // current page; only a normal data query replaces the query tab pager.
        if is_run {
            self.pager = pager;
        }
        let count_base = is_run
            .then(|| self.pager.as_ref().map(|pager| pager.base_sql.clone()))
            .flatten();
        self.execute_query(
            conn.clone(),
            effective_sql,
            title_sql,
            is_run,
            page_request,
            result_handle,
            plan_seq,
            cx,
        );
        if let Some(base_sql) = count_base {
            self.spawn_total_count(conn, base_sql, cx);
        }
    }

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
            if let Err(error) = &formatted {
                error!(
                    operation = "sql_format",
                    sql_bytes = source_sql.len(),
                    error = %error,
                    "format SQL failed"
                );
            }
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
                        this.invalidate_query_context(cx);
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

    /// 停止客户端等待，并请求取消服务端查询。
    pub(super) fn handle_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_task.take().is_none() {
            return;
        }
        let cancel_seq = self.run_seq.wrapping_add(1);
        self.run_seq = cancel_seq;
        self.clear_cancelled_result(cx);
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
                    if this.run_seq != cancel_seq {
                        return;
                    }
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
                    Ok(()) => info!(
                        operation = "sql_query_cancel",
                        connection_id = %conn.id,
                        driver = ?conn.driver,
                        thread_id = tid,
                        "server query cancellation confirmed"
                    ),
                    Err(e) => tracing::warn!(
                        operation = "sql_query_cancel",
                        connection_id = %conn.id,
                        driver = ?conn.driver,
                        thread_id = tid,
                        error = %e,
                        "server query cancellation failed"
                    ),
                }
            })
            .detach();
        } else {
            window.push_notification(
                Notification::info("已停止等待；未获取到服务器线程，语句可能仍在服务器执行")
                    .autohide(true),
                cx,
            );
        }
        self.running = false;
        self.query_start = None;
        info!(
            operation = "sql_query_cancel",
            "client query wait cancelled"
        );
        cx.notify();
    }

    /// Clears only the result panel owned by the cancelled request.
    pub(super) fn clear_cancelled_result(&mut self, cx: &mut Context<Self>) {
        match self.running_target.take() {
            Some(QueryResultTarget::Data) => {
                self.pager = None;
                self.result.update(cx, |result, cx| {
                    result.set_state(ResultState::Empty, cx);
                });
            }
            Some(QueryResultTarget::Plan) => {
                self.plan_result.update(cx, |result, cx| {
                    result.set_state(ResultState::Empty, cx);
                });
            }
            None => {}
        }
    }
}

fn parse_count_result(result: &QueryResult) -> Option<u64> {
    match result.rows.first()?.values.first()? {
        Value::Int(n) => u64::try_from(*n).ok(),
        Value::Text(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

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
