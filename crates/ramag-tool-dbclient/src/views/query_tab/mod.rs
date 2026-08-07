mod actions;
mod examples;
mod paging;
mod render;
mod sql_utils;

pub(crate) use examples::sql_examples;

use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use parking_lot::RwLock;

use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, MAX_SQL_QUERY_BYTES};
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
    /// 查询请求代际；取消后快速重跑时，旧回包与旧耗时 ticker 不得跟随新查询。
    pub(super) run_seq: u64,
    /// 计数代际：仅新查询递增、翻页不变，使后台 COUNT 回包跨翻页仍有效、被新查询作废。
    pub(super) count_seq: u64,
    /// SQL 格式化防重入；CPU 工作在共享有界 worker 中执行。
    pub(super) formatting: bool,
    pub(super) current_task: Option<Task<()>>,
    /// 编辑器停顿后再预拉列元数据；替换句柄会取消尚未触发的旧任务。
    pub(super) column_prefetch_task: Option<Task<()>>,
    /// 取消句柄：driver 在 acquire 后写入 mysql 后端 thread id（0 = 未拿到）
    pub(super) cancel_handle: Option<ramag_domain::traits::CancelHandle>,
    pub(super) query_start: Option<Instant>,
    pub(super) schema_cache: Arc<RwLock<SchemaCache>>,
    pub(super) title: String,
    pub(super) short_title: Option<String>,
    /// 异步回调无法访问 Window，通知由 Render 延后推送。
    pub(super) pending_notification: Option<Notification>,
    /// 上游显式指定的目标表 (schema, table)：表树点击触发的 SELECT 才有
    pub(super) pinned_target: Option<(String, String)>,
    pub(super) show_editor: bool,
    /// 分页状态：本次 run 命中"未手写 LIMIT 的单条 SELECT"时为 Some
    pager: Option<paging::Pager>,
    /// 上次自动注入的 SQL（表树浏览 / 示例）。编辑器内容仍与之相等 = 用户未手改，
    /// 表树切表可安全原地覆盖；否则视为手写草稿，浏览须另开 Tab（防丢稿）
    pub(super) last_injected_sql: Option<String>,
    pub(super) _editor_sub: gpui::Subscription,
    pub(super) _result_sub: gpui::Subscription,
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
                // 手改 SQL 即失去"表树单表数据"资格：清目标表，当前结果立即转只读
                // （程序注入走 set_value 不发 Change，不会误触）
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
                ResultPanelEvent::RowSearchChanged => cx.notify(),
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
            query_start: None,
            schema_cache,
            title: title.into(),
            short_title: None,
            pending_notification: None,
            pinned_target: None,
            show_editor: true,
            pager: None,
            last_injected_sql: None,
            _editor_sub: editor_sub,
            _result_sub: result_sub,
        }
    }

    pub fn set_result_active(&self, active: bool, cx: &mut Context<Self>) {
        self.result
            .update(cx, |result, _| result.set_result_active(active));
    }

    /// 是否存在用户手写草稿：编辑器非空且内容不等于上次自动注入的 SQL。
    /// 表树切表据此决定原地覆盖还是另开 Tab
    pub fn has_user_draft(&self, cx: &gpui::App) -> bool {
        let value = self.editor.read(cx).value();
        let cur = value.trim();
        if cur.is_empty() {
            return false;
        }
        self.last_injected_sql.as_deref().map(str::trim) != Some(cur)
    }

    /// 手写草稿快照；自动注入或空编辑器不参与跨重启持久化。
    pub fn draft_text(&self, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value())
    }

    /// 从本地偏好恢复手写草稿，不触发查询执行。
    pub fn restore_draft(
        &mut self,
        text: gpui::SharedString,
        schema: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sql(text, window, cx);
        self.active_schema = schema.filter(|s| !s.is_empty());
        // 恢复内容必须被视为用户草稿，关闭时继续受保护。
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
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        self.connection = conn.clone();
        // 旧连接的分页状态不能带到新连接（base_sql 已不可信）
        self.clear_pager(cx);
        // 同步给 ResultPanel：单元格编辑弹框需要最新的连接来发 UPDATE
        let svc = self.service.clone();
        self.result.update(cx, |r, _| {
            r.set_executor(Some(svc), conn);
        });
        cx.notify();
    }

    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema != normalized {
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
        // 用户改了 SQL 就清掉之前的 pinned_target：行内编辑不应再用旧目标表
        self.pinned_target = None;
        // 编辑器被整体替换后旧分页状态作废（避免"下一页"重跑已被换掉的 SQL）
        // 默认视为普通写入（可能是历史填入等用户内容）；自动注入路径由调用方再 mark_injected
        self.last_injected_sql = None;
        // set_value 不发 InputEvent::Change（emit_events=false），手动触发预拉
        self.prefetch_columns_now(cx);
        cx.notify();
    }

    pub fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.handle_run(window, cx);
    }

    /// 关闭 Tab 前调用：查询执行中先取消（drop 客户端任务 + 发后端 KILL），
    /// 避免 Tab 关掉后语句仍占用数据库资源
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
        // 示例不是表树的单表浏览结果，不能沿用旧表的行级增删改目标或分页基线。
        self.pinned_target = None;
        // 示例模板属自动注入：未手改前点表树仍可原地覆盖
        self.last_injected_sql = Some(sql.to_string());
        // set_value 不发 Change 事件，手动触发列结构预拉（与 set_sql 一致）
        self.prefetch_columns_now(cx);
        cx.notify();
    }

    /// SQL、连接或 schema 改变后，旧页码与分页基线必须一起失效。
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
