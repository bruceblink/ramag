//! SQL 查询结果的跨连接只读比较。

use std::sync::Arc;

use gpui::{AppContext as _, Context, ParentElement as _, Window};
use gpui_component::{WindowExt as _, notification::Notification};
use ramag_domain::entities::{
    ConnectionConfig, DriverKind, MAX_CONNECTION_IDENTIFIER_BYTES, Query, QueryResult,
};
use ramag_infra_sql_shared::sql::is_write_statement;

use super::QueryTab;
use super::paging::{page_sql, trim_page_sentinel};
use crate::views::connection_target::{
    has_sql_compare_target, resolve_sql_connection, sql_connection_hint,
};
use crate::views::result_diff::{ResultScopeKey, ResultSnapshot};
use crate::views::result_diff_dialog::ResultDiffDialog;

const MAX_CROSS_COMPARE_RESULT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct CrossCompareRequest {
    source: ResultSnapshot,
    source_connection: ConnectionConfig,
    source_sql: String,
    source_schema: Option<String>,
    page: Option<CrossComparePage>,
    context_generation: u64,
    source_result_revision: u64,
}

#[derive(Debug, Clone)]
struct CrossComparePage {
    page: usize,
    page_size: usize,
    scope_key: ResultScopeKey,
    base_sql: String,
}

impl QueryTab {
    pub(super) fn can_compare_cross_connection(&self, cx: &gpui::App) -> bool {
        if self.cross_compare_running {
            return false;
        }
        self.cross_compare_request(cx).is_ok_and(|request| {
            self.connection_list.as_ref().is_some_and(|list| {
                has_sql_compare_target(&request.source_connection, list.read(cx).connections())
            })
        })
    }

    /// Opens a selector for another same-driver SQL connection and reruns the current read query there.
    pub(super) fn prompt_cross_connection_result_compare(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = match self.cross_compare_request(cx) {
            Ok(request) => request,
            Err(message) => {
                self.pending_notification = Some(Notification::warning(message).autohide(true));
                cx.notify();
                return;
            }
        };
        let Some(connection_list) = self.connection_list.as_ref() else {
            return;
        };
        let available_connections = connection_list.read(cx).connections().to_vec();
        if !has_sql_compare_target(&request.source_connection, &available_connections) {
            self.pending_notification = Some(
                Notification::warning("没有可用的同类型目标连接，请先添加另一个 SQL 连接")
                    .autohide(true),
            );
            cx.notify();
            return;
        }
        let hint = sql_connection_hint(&request.source_connection, &available_connections);
        let entity = cx.entity().clone();
        let source_connection = request.source_connection.clone();
        ramag_ui::open_bounded_prompt(
            "跨连接比较结果",
            format!("输入目标连接名称或 ID。当前 SQL 只读重放，不写入目标连接。可用连接：{hint}"),
            "",
            "比较",
            MAX_CONNECTION_IDENTIFIER_BYTES,
            move |selector, window, app| {
                let target = match resolve_sql_connection(
                    &selector,
                    &source_connection,
                    &available_connections,
                ) {
                    Ok(target) => target,
                    Err(message) => {
                        window
                            .push_notification(Notification::warning(message).autohide(true), app);
                        return;
                    }
                };
                if target.id == source_connection.id {
                    window.push_notification(
                        Notification::warning("目标连接必须不同于当前连接").autohide(true),
                        app,
                    );
                    return;
                }
                entity.update(app, |tab, cx| {
                    tab.start_cross_connection_comparison(request.clone(), target, window, cx);
                });
            },
            window,
            cx,
        );
    }

    fn cross_compare_request(&self, cx: &gpui::App) -> Result<CrossCompareRequest, String> {
        let Some(source_connection) = self.connection.clone() else {
            return Err("尚未选择 SQL 连接".into());
        };
        if !matches!(
            source_connection.driver,
            DriverKind::Mysql | DriverKind::Postgres
        ) {
            return Err("当前数据库暂不支持 SQL 连接对比".into());
        }
        let result = self.result.read(cx);
        let Some(source) = result.current_comparison_snapshot("源") else {
            return Err("请先执行一个结果查询".into());
        };
        let Some(source_sql) = result.source_sql() else {
            return Err("当前结果没有可重放的 SQL".into());
        };
        if source_sql.trim().is_empty() {
            return Err("当前结果没有可重放的 SQL".into());
        }
        if is_write_statement(&source_sql) {
            return Err("当前结果来自写操作，不能在其他连接上重放".into());
        }
        let page = self.cross_compare_page(&result, source.scope_key);
        Ok(CrossCompareRequest {
            source_result_revision: result.result_revision(),
            source_schema: result.source_schema(),
            source_sql,
            source,
            source_connection,
            page,
            context_generation: self.cross_compare_seq,
        })
    }

    fn cross_compare_page(
        &self,
        result: &crate::views::result_panel::ResultPanel,
        source_scope_key: ResultScopeKey,
    ) -> Option<CrossComparePage> {
        let pager = self.pager.as_ref()?;
        let pagination = result.pagination()?;
        (pager.page == pagination.page && pager.page_size == pagination.page_size).then(|| {
            CrossComparePage {
                page: pagination.page,
                page_size: pagination.page_size,
                scope_key: ResultScopeKey {
                    page: Some(pagination.page),
                    page_size: Some(pagination.page_size),
                    truncated: source_scope_key.truncated,
                },
                base_sql: pager.base_sql.clone(),
            }
        })
    }

    fn start_cross_connection_comparison(
        &mut self,
        request: CrossCompareRequest,
        target_connection: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cross_compare_running {
            return;
        }
        if self.cross_compare_seq != request.context_generation {
            self.pending_notification =
                Some(Notification::warning("查询结果已变化，请重新打开跨连接比较").autohide(true));
            cx.notify();
            return;
        }
        if self.result.read(cx).result_revision() != request.source_result_revision {
            self.pending_notification =
                Some(Notification::warning("查询结果已变化，请重新打开跨连接比较").autohide(true));
            cx.notify();
            return;
        }
        self.cross_compare_seq = self.cross_compare_seq.wrapping_add(1);
        let request_generation = self.cross_compare_seq;
        self.cross_compare_running = true;
        cx.notify();

        let source_sql = request.source_sql.clone();
        let mut target_sql = source_sql.clone();
        if let Some(page) = &request.page {
            target_sql = match page_sql(&page.base_sql, page.page_size, page.page) {
                Ok(sql) => sql,
                Err(message) => {
                    self.cross_compare_running = false;
                    self.pending_notification = Some(Notification::error(message).autohide(true));
                    cx.notify();
                    return;
                }
            };
        }
        let mut query = Query::new(target_sql);
        if let Some(schema) = request.source_schema.clone() {
            query = query.with_schema(schema);
        }
        query = query.with_result_byte_limit(MAX_CROSS_COMPARE_RESULT_BYTES);

        let service = self.service.clone();
        let source_connection = request.source_connection.clone();
        let source_snapshot = request.source;
        let source_identity_columns = source_snapshot.identity_columns.clone();
        let source_pinned_target = source_snapshot.context.pinned_target.clone();
        let source_schema = request.source_schema;
        let page = request.page;
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let outcome = service.execute(&target_connection, &query).await;
            let target_snapshot = outcome.map(|mut result| {
                if let Some(page) = page.as_ref() {
                    trim_page_sentinel(&mut result, page.page_size);
                }
                let (scope, scope_key) = cross_compare_scope(&result, page.as_ref());
                ResultSnapshot::from_query(
                    Arc::new(result),
                    "目标",
                    Some(&target_connection),
                    Some(&source_sql),
                    scope,
                    scope_key,
                    source_pinned_target,
                    source_identity_columns,
                    source_schema,
                )
            });
            if let Err(error) = &target_snapshot {
                tracing::warn!(
                    operation = "sql_cross_connection_compare",
                    source_connection_id = %source_connection.id,
                    target_connection_id = %target_connection.id,
                    driver = ?target_connection.driver,
                    error = %error,
                    "cross-connection result query failed"
                );
            }
            let _ = cx.update_window(window_handle, |_, window, app| {
                this.update(app, |this, cx| {
                    if this.cross_compare_seq != request_generation {
                        return;
                    }
                    this.cross_compare_running = false;
                    match target_snapshot {
                        Ok(target_snapshot) => {
                            this.open_cross_connection_result_dialog(
                                source_connection,
                                target_connection,
                                source_snapshot,
                                target_snapshot,
                                window,
                                cx,
                            );
                        }
                        Err(error) => {
                            this.pending_notification = Some(
                                Notification::error(format!("目标连接查询失败：{error}"))
                                    .autohide(true),
                            );
                        }
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }

    fn open_cross_connection_result_dialog(
        &mut self,
        source_connection: ConnectionConfig,
        target_connection: ConnectionConfig,
        source: ResultSnapshot,
        target: ResultSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.new(|cx| ResultDiffDialog::new(source, target, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let panel_for_content = panel.clone();
            dialog
                .title(format!(
                    "查询结果差异 · {} → {}",
                    source_connection.name, target_connection.name
                ))
                .width(gpui::px(1_120.0))
                .margin_top(gpui::px(55.0))
                .content(move |content, _, _| content.child(panel_for_content.clone()))
        });
    }
}

fn cross_compare_scope(
    result: &QueryResult,
    page: Option<&CrossComparePage>,
) -> (String, ResultScopeKey) {
    if let Some(page) = page {
        let mut scope_key = page.scope_key;
        scope_key.truncated = result.truncated;
        return (
            format!(
                "第 {} 页 · 已加载 {} 行{}",
                page.page.saturating_add(1),
                result.rows.len(),
                if result.truncated {
                    " · 结果已截断"
                } else {
                    ""
                }
            ),
            scope_key,
        );
    }
    (
        format!(
            "已加载 {} 行{}",
            result.rows.len(),
            if result.truncated {
                " · 结果已截断"
            } else {
                ""
            }
        ),
        ResultScopeKey {
            truncated: result.truncated,
            ..ResultScopeKey::default()
        },
    )
}
