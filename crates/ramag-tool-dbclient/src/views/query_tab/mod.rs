mod actions;
mod comparison;
mod comparison_toolbar;
mod examples;
mod navigation;
mod paging;
mod render;
mod render_helpers;
mod sql_utils;
mod subscriptions;
mod toolbar;
mod transaction;

pub(crate) use examples::sql_examples;

use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, Window};
use gpui_component::input::InputState;
use gpui_component::notification::Notification;
use parking_lot::RwLock;

use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, MAX_SQL_QUERY_BYTES, TransactionId};
use ramag_ui::ResultMemoryBudget;
use ramag_ui::platform::primary_shortcut;

use crate::sql_completion::SchemaCache;
use crate::views::connection_list::ConnectionListPanel;
use crate::views::result_panel::{ResultPanel, ResultState};

#[cfg(test)]
mod tests;

pub struct QueryTab {
    pub(super) service: Arc<ConnectionService>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) connection_list: Option<Entity<ConnectionListPanel>>,
    pub(super) active_schema: Option<String>,
    pub(super) editor: Entity<InputState>,
    pub(super) result: Entity<ResultPanel>,
    /// 独立的执行计划结果面板；生成计划不会覆盖数据结果。
    pub(super) plan_result: Entity<ResultPanel>,
    /// 当前是否显示执行计划结果。
    pub(super) show_plan: bool,
    /// 当前查询标签是否可见，用于在两个结果面板之间转移内存活跃状态。
    pub(super) result_active: bool,
    pub(super) running: bool,
    /// 当前异步请求写入的数据或计划面板；取消时只清理这个面板。
    pub(super) running_target: Option<QueryResultTarget>,
    /// 查询代际，忽略取消后的旧回包和计时。
    pub(super) run_seq: u64,
    /// 执行计划代际；SQL 上下文变化后旧计划回包不能重新出现。
    pub(super) plan_seq: u64,
    /// COUNT 代际；新查询使旧回包失效，翻页不变。
    pub(super) count_seq: u64,
    pub(super) formatting: bool,
    pub(super) current_task: Option<Task<()>>,
    /// 延后预取列元数据；替换任务会取消旧任务。
    pub(super) column_prefetch_task: Option<Task<()>>,
    /// 取消句柄；acquire 后写入 MySQL 后端线程 ID。
    pub(super) cancel_handle: Option<ramag_domain::traits::CancelHandle>,
    /// Open manual transaction for generated row mutations.
    pub(super) transaction: Option<TransactionSession>,
    /// Prevents row mutations while transaction control is in flight.
    pub(super) transaction_busy: bool,
    /// Last transaction error that still needs an explicit rollback or restart.
    pub(super) transaction_error: Option<String>,
    /// Invalidates late begin/commit/rollback responses after context changes.
    pub(super) transaction_seq: u64,
    pub(super) query_start: Option<Instant>,
    /// 跨连接结果查询代次；上下文变化后丢弃目标连接的迟到回包。
    pub(super) cross_compare_seq: u64,
    pub(super) cross_compare_running: bool,
    pub(super) schema_cache: Arc<RwLock<SchemaCache>>,
    pub(super) title: String,
    pub(super) short_title: Option<String>,
    pub(super) pending_notification: Option<Notification>,
    pub(super) pinned_target: Option<(String, String)>,
    pub(super) show_editor: bool,
    /// 未手写 LIMIT 的单条 SELECT 分页状态。
    pager: Option<paging::Pager>,
    /// 当前查询标签的页大小偏好；新查询复用上次选择的值。
    page_size: usize,
    /// 上次自动注入的 SQL；不同即视为用户草稿。
    pub(super) last_injected_sql: Option<String>,
    pub(super) _editor_sub: gpui::Subscription,
    pub(super) _result_sub: gpui::Subscription,
    pub(super) _plan_result_sub: gpui::Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryResultTarget {
    Data,
    Plan,
}

impl QueryResultTarget {
    /// Maps an execution request to the result panel it owns.
    pub(super) fn from_plan_request(plan_seq: Option<u64>) -> Self {
        if plan_seq.is_some() {
            Self::Plan
        } else {
            Self::Data
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TransactionSession {
    pub(super) id: TransactionId,
    pub(super) dirty: bool,
    pub(super) savepoints: Vec<TransactionSavepoint>,
    pub(super) next_savepoint: u32,
}

#[derive(Debug, Clone)]
pub(super) struct TransactionSavepoint {
    pub(super) name: String,
    pub(super) dirty: bool,
}

#[derive(Debug, Clone)]
pub enum QueryTabEvent {
    DraftChanged,
    LocateTableRequested {
        schema: String,
        table: String,
    },
    TableImportRequested {
        schema: String,
        table: String,
        policy: ramag_domain::entities::ConflictPolicy,
        files: Vec<std::path::PathBuf>,
    },
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        service: Arc<ConnectionService>,
        title: impl Into<String>,
        connection: Option<ConnectionConfig>,
        schema_cache: Arc<RwLock<SchemaCache>>,
        result_memory: ResultMemoryBudget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let cache_for_provider = schema_cache.clone();
        let editor = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .code_editor("sql")
                .multi_line(true)
                .line_number(true)
                .placeholder(format!(
                    "-- 输入 SQL，按 {} 运行\nSELECT 1;",
                    primary_shortcut("Enter")
                ))
                .rows(8);
            state.lsp.completion_provider = Some(
                crate::sql_completion::SqlCompletionProvider::new_rc(cache_for_provider),
            );
            state
        });
        let cache_for_result = schema_cache.clone();
        let result = cx.new(|cx| {
            let mut p = ResultPanel::new(window, cx);
            p.set_executor(Some(service.clone()), connection.clone());
            p.set_schema_cache(Some(cache_for_result));
            p
        });
        let weak_result = result.downgrade();
        let lease = result_memory.register(move |app| {
            weak_result
                .update(app, |panel, cx| panel.evict_result_for_budget(cx))
                .is_ok()
        });
        result.update(cx, |panel, _| panel.attach_result_memory(lease));

        let cache_for_plan = schema_cache.clone();
        let plan_result = cx.new(|cx| {
            let mut panel = ResultPanel::new(window, cx);
            panel.set_plan_mode(true);
            panel.set_executor(Some(service.clone()), connection.clone());
            panel.set_schema_cache(Some(cache_for_plan));
            panel
        });
        let weak_plan_result = plan_result.downgrade();
        let plan_lease = result_memory.register(move |app| {
            weak_plan_result
                .update(app, |panel, cx| panel.evict_result_for_budget(cx))
                .is_ok()
        });
        plan_result.update(cx, |panel, _| panel.attach_result_memory(plan_lease));

        let (editor_sub, result_sub, plan_result_sub) =
            subscriptions::subscribe(&editor, &result, &plan_result, window, cx);

        let initial_schema = connection
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        Self {
            service,
            connection,
            connection_list: None,
            active_schema: initial_schema,
            editor,
            result,
            plan_result,
            show_plan: false,
            result_active: false,
            running: false,
            running_target: None,
            run_seq: 0,
            plan_seq: 0,
            count_seq: 0,
            formatting: false,
            current_task: None,
            column_prefetch_task: None,
            cancel_handle: None,
            transaction: None,
            transaction_busy: false,
            transaction_error: None,
            transaction_seq: 0,
            query_start: None,
            cross_compare_seq: 0,
            cross_compare_running: false,
            schema_cache,
            title: title.into(),
            short_title: None,
            pending_notification: None,
            pinned_target: None,
            show_editor: true,
            pager: None,
            page_size: ramag_ui::DEFAULT_RESULT_PAGE_SIZE,
            last_injected_sql: None,
            _editor_sub: editor_sub,
            _result_sub: result_sub,
            _plan_result_sub: plan_result_sub,
        }
    }

    pub fn set_result_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.result_active = active;
        self.result.update(cx, |result, _| {
            result.set_result_active(active && !self.show_plan)
        });
        self.plan_result.update(cx, |result, _| {
            result.set_result_active(active && self.show_plan)
        });
    }

    /// Switches between data and plan results without changing either panel's stored state.
    pub(super) fn set_plan_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.plan_result_is_empty(cx) {
            return;
        }
        if visible && !self.guard_no_pending_result_edits("切换到执行计划", cx) {
            return;
        }
        if self.show_plan == visible {
            return;
        }
        self.show_plan = visible;
        self.set_result_active(self.result_active, cx);
        cx.notify();
    }

    /// Returns the panel currently visible to result-toolbar actions.
    pub(super) fn active_result(&self) -> Entity<ResultPanel> {
        if self.show_plan {
            self.plan_result.clone()
        } else {
            self.result.clone()
        }
    }

    /// Reports whether a plan has been generated and can be selected.
    pub(super) fn plan_result_is_empty(&self, cx: &gpui::App) -> bool {
        matches!(
            self.plan_result.read(cx).state(),
            ResultState::Empty | ResultState::Released(_)
        )
    }

    pub fn has_user_draft(&self, cx: &gpui::App) -> bool {
        let value = self.editor.read(cx).value();
        let cur = value.trim();
        if cur.is_empty() {
            return false;
        }
        self.last_injected_sql.as_deref().map(str::trim) != Some(cur)
    }

    pub fn draft_text(&self, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value())
    }

    pub fn restore_draft(
        &mut self,
        text: gpui::SharedString,
        schema: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.set_sql(text, window, cx) {
            return;
        }
        self.active_schema = schema.filter(|s| !s.is_empty());
        self.last_injected_sql = None;
    }

    pub fn mark_injected(&mut self, sql: String) {
        self.last_injected_sql = Some(sql);
    }

    pub fn set_show_editor(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.show_editor != v {
            self.show_editor = v;
            cx.notify();
        }
    }

    pub fn clear_result_column_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.result
            .update(cx, |r, cx| r.clear_column_filter(window, cx));
    }

    /// Updates the pinned table target and invalidates stale query/plan state.
    pub fn set_pinned_target(&mut self, target: Option<(String, String)>, cx: &mut Context<Self>) {
        self.pinned_target = target;
        self.invalidate_query_context(cx);
    }

    pub fn display_title(&self) -> &str {
        self.short_title.as_deref().unwrap_or(&self.title)
    }

    pub fn set_connection(&mut self, conn: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        if self.has_open_transaction() {
            self.rollback_transaction_detached(cx);
        } else {
            self.transaction_error = None;
        }
        self.clear_pager(cx);
        self.invalidate_query_context(cx);
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        self.connection = conn.clone();
        let svc = self.service.clone();
        self.result.update(cx, |r, cx| {
            r.clear_pending_cell_edits(cx);
            r.clear_editable_target(cx);
            r.set_executor(Some(svc), conn);
        });
        let svc = self.service.clone();
        let conn_for_plan = self.connection.clone();
        self.plan_result.update(cx, |r, _| {
            r.set_executor(Some(svc), conn_for_plan);
        });
        cx.notify();
    }

    pub(super) fn set_connection_list(
        &mut self,
        connection_list: Option<Entity<ConnectionListPanel>>,
    ) {
        self.connection_list = connection_list;
    }

    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema != normalized {
            if self.has_open_transaction() {
                self.rollback_transaction_detached(cx);
            } else {
                self.transaction_error = None;
            }
            self.active_schema = normalized;
            self.clear_pager(cx);
            self.invalidate_query_context(cx);
            self.result.update(cx, |r, cx| {
                r.clear_pending_cell_edits(cx);
                r.clear_editable_target(cx);
            });
            cx.notify();
        }
    }

    pub fn set_sql(
        &mut self,
        sql: impl Into<gpui::SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let sql = sql.into();
        if !self.guard_no_pending_result_edits("替换 SQL", cx) {
            return false;
        }
        self.clear_pager(cx);
        self.invalidate_query_context(cx);
        if sql.len() > MAX_SQL_QUERY_BYTES {
            self.result.update(cx, |result, cx| {
                result.set_state(
                    crate::views::result_panel::ResultState::Error(format!(
                        "SQL 内容超过 {} MiB 安全上限，未写入编辑器",
                        MAX_SQL_QUERY_BYTES / 1024 / 1024
                    )),
                    cx,
                );
            });
            return false;
        }
        self.editor
            .update(cx, |state, cx| state.set_value(sql, window, cx));
        self.pinned_target = None;
        self.result.update(cx, |r, cx| {
            r.clear_pending_cell_edits(cx);
            r.clear_editable_target(cx);
        });
        self.last_injected_sql = None;
        // set_value 不发 Change，需手动预取列结构。
        self.prefetch_columns_now(cx);
        cx.notify();
        true
    }

    pub fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.handle_run(window, cx);
    }

    pub fn cancel_if_running(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            self.handle_cancel(window, cx);
        }
    }

    pub(super) fn insert_example(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.guard_no_pending_result_edits("替换 SQL", cx) {
            return;
        }
        self.clear_pager(cx);
        self.invalidate_query_context(cx);
        self.editor.update(cx, |state, cx| {
            state.set_value(sql.to_string(), window, cx);
            state.focus(window, cx);
        });
        self.pinned_target = None;
        self.last_injected_sql = Some(sql.to_string());
        // set_value 不发 Change，需手动预取列结构。
        self.prefetch_columns_now(cx);
        cx.notify();
    }

    pub(super) fn clear_pager(&mut self, cx: &mut Context<Self>) {
        self.pager = None;
        self.count_seq = self.count_seq.wrapping_add(1);
        self.result
            .update(cx, |result, cx| result.set_pagination(None, cx));
    }

    /// Invalidates in-flight query and COUNT responses when the console context changes.
    /// Completed results remain visible, while a running placeholder is cleared so the tab
    /// cannot stay blocked by a request that no longer belongs to the current SQL context.
    pub(super) fn invalidate_query_context(&mut self, cx: &mut Context<Self>) {
        self.cancel_inflight_query(cx);
        self.run_seq = self.run_seq.wrapping_add(1);
        self.count_seq = self.count_seq.wrapping_add(1);
        self.show_plan = false;
        self.plan_seq = self.plan_seq.wrapping_add(1);
        self.running = false;
        self.running_target = None;
        self.current_task = None;
        self.cancel_handle = None;
        self.query_start = None;
        self.cross_compare_seq = self.cross_compare_seq.wrapping_add(1);
        self.cross_compare_running = false;
        let result_active = self.result_active;
        self.set_result_active(result_active, cx);
        self.result.update(cx, |result, cx| {
            if matches!(result.state(), ResultState::Running) {
                result.set_state(ResultState::Empty, cx);
            }
        });
        self.plan_result.update(cx, |result, cx| {
            if !matches!(result.state(), ResultState::Empty) {
                result.set_state(ResultState::Empty, cx);
            }
        });
        cx.notify();
    }

    /// Requests server-side cancellation before dropping an invalidated SQL request.
    /// The UI still relies on `run_seq` to reject any late response, while this best-effort
    /// request prevents a query from continuing after its console context was replaced.
    fn cancel_inflight_query(&mut self, cx: &mut Context<Self>) {
        if !self.running {
            return;
        }
        let _ = self.current_task.take();
        let Some(cancel_handle) = self.cancel_handle.take() else {
            return;
        };
        let thread_id = cancel_handle.load(std::sync::atomic::Ordering::SeqCst);
        let Some(conn) = self.connection.clone() else {
            return;
        };
        if thread_id == 0 {
            return;
        }
        let service = self.service.clone();
        cx.background_spawn(async move {
            match service.cancel_query(&conn, thread_id).await {
                Ok(()) => tracing::info!(
                    operation = "sql_query_context_cancel",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    thread_id,
                    "invalidated query cancellation confirmed"
                ),
                Err(error) => tracing::warn!(
                    operation = "sql_query_context_cancel",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    thread_id,
                    error = %error,
                    "invalidated query cancellation failed"
                ),
            }
        })
        .detach();
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }
}
