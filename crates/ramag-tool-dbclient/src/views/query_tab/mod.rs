mod actions;
mod examples;
mod paging;
mod render;
mod sql_utils;
mod transaction;

pub(crate) use examples::sql_examples;

use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use parking_lot::RwLock;

use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, MAX_SQL_QUERY_BYTES, TransactionId};
use ramag_ui::ResultMemoryBudget;
use ramag_ui::platform::primary_shortcut;

use crate::sql_completion::SchemaCache;
use crate::views::result_panel::{ResultPanel, ResultPanelEvent};

pub struct QueryTab {
    pub(super) service: Arc<ConnectionService>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) active_schema: Option<String>,
    pub(super) editor: Entity<InputState>,
    pub(super) result: Entity<ResultPanel>,
    pub(super) running: bool,
    /// 查询代际，忽略取消后的旧回包和计时。
    pub(super) run_seq: u64,
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
    /// Invalidates late begin/commit/rollback responses after context changes.
    pub(super) transaction_seq: u64,
    pub(super) query_start: Option<Instant>,
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
}

#[derive(Debug, Clone)]
pub(super) struct TransactionSession {
    pub(super) id: TransactionId,
    pub(super) dirty: bool,
}

#[derive(Debug, Clone)]
pub enum QueryTabEvent {
    DraftChanged,
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

        let editor_for_sub = editor.clone();
        let editor_sub = cx.subscribe_in(
            &editor,
            window,
            move |this: &mut Self, _, e: &InputEvent, window, cx| {
                if !matches!(e, InputEvent::Change) {
                    return;
                }
                this.clear_pager(cx);
                if ramag_ui::clamp_multiline_input_value(
                    &editor_for_sub,
                    MAX_SQL_QUERY_BYTES,
                    window,
                    cx,
                ) {
                    this.pending_notification = Some(
                        Notification::warning(format!(
                            "SQL 编辑器最多保留 {} MiB，超出部分已截断",
                            MAX_SQL_QUERY_BYTES / 1024 / 1024
                        ))
                        .autohide(true),
                    );
                }
                if this.pinned_target.is_some() && this.has_user_draft(cx) {
                    this.pinned_target = None;
                    this.result.update(cx, |r, cx| r.clear_editable_target(cx));
                }
                this.schedule_column_prefetch(cx);
                cx.emit(QueryTabEvent::DraftChanged);
            },
        );
        let result_sub = cx.subscribe(
            &result,
            |this: &mut Self, _, event: &ResultPanelEvent, cx| match event {
                ResultPanelEvent::PageRequested(page) => this.handle_page(*page, cx),
                ResultPanelEvent::PageSizeChanged(page_size) => {
                    this.handle_page_size(*page_size, cx)
                }
                ResultPanelEvent::RowSearchChanged => cx.notify(),
                ResultPanelEvent::MutationCompleted => this.mark_transaction_dirty(cx),
            },
        );

        let initial_schema = connection
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        Self {
            service,
            connection,
            active_schema: initial_schema,
            editor,
            result,
            running: false,
            run_seq: 0,
            count_seq: 0,
            formatting: false,
            current_task: None,
            column_prefetch_task: None,
            cancel_handle: None,
            transaction: None,
            transaction_busy: false,
            transaction_seq: 0,
            query_start: None,
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
        }
    }

    pub fn set_result_active(&self, active: bool, cx: &mut Context<Self>) {
        self.result
            .update(cx, |result, _| result.set_result_active(active));
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
        self.set_sql(text, window, cx);
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

    pub fn set_pinned_target(&mut self, target: Option<(String, String)>) {
        self.pinned_target = target;
    }

    pub fn display_title(&self) -> &str {
        self.short_title.as_deref().unwrap_or(&self.title)
    }

    pub fn set_connection(&mut self, conn: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        if self.has_open_transaction() {
            self.rollback_transaction_detached(cx);
        }
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        self.connection = conn.clone();
        self.clear_pager(cx);
        let svc = self.service.clone();
        self.result.update(cx, |r, _| {
            r.set_executor(Some(svc), conn);
        });
        cx.notify();
    }

    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema != normalized {
            if self.has_open_transaction() {
                self.rollback_transaction_detached(cx);
            }
            self.active_schema = normalized;
            self.clear_pager(cx);
            cx.notify();
        }
    }

    pub fn set_sql(
        &mut self,
        sql: impl Into<gpui::SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sql = sql.into();
        self.clear_pager(cx);
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
            return;
        }
        self.editor
            .update(cx, |state, cx| state.set_value(sql, window, cx));
        self.pinned_target = None;
        self.last_injected_sql = None;
        // set_value 不发 Change，需手动预取列结构。
        self.prefetch_columns_now(cx);
        cx.notify();
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
        self.clear_pager(cx);
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
        self.result
            .update(cx, |result, cx| result.set_pagination(None, cx));
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }
}
