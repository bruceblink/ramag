//! 单个查询标签：编辑器 + 工具条 + 结果面板

mod actions;
mod examples;
mod paging;
mod render;
mod sql_utils;

// QueryPanel 的 Tab 栏「示例」下拉用
pub(crate) use examples::sql_examples;

use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use parking_lot::RwLock;

use ramag_app::ConnectionService;
use ramag_domain::entities::ConnectionConfig;
use ramag_ui::platform::primary_shortcut;

use crate::sql_completion::SchemaCache;
use crate::views::result_panel::ResultPanel;

/// 单个查询标签
pub struct QueryTab {
    pub(super) service: Arc<ConnectionService>,
    /// 当前激活的连接（None 时禁用执行）
    pub(super) connection: Option<ConnectionConfig>,
    /// 当前激活的默认库；表树点击表/schema 时由父 session 同步进来
    pub(super) active_schema: Option<String>,
    /// SQL 编辑器
    pub(super) editor: Entity<InputState>,
    /// 结果面板
    pub(super) result: Entity<ResultPanel>,
    /// 是否在执行中
    pub(super) running: bool,
    /// SQL 格式化防重入；CPU 工作在共享有界 worker 中执行。
    pub(super) formatting: bool,
    /// 当前正在跑的任务句柄（drop 后取消异步任务）
    pub(super) current_task: Option<Task<()>>,
    /// 取消句柄：driver 在 acquire 后写入 mysql 后端 thread id（0 = 未拿到）
    pub(super) cancel_handle: Option<ramag_domain::traits::CancelHandle>,
    /// 查询开始时间，仅 running 时为 Some
    pub(super) query_start: Option<Instant>,
    /// 与编辑器 / 表树共享的补全 schema 缓存（用于 DDL 后自动刷新）
    pub(super) schema_cache: Arc<RwLock<SchemaCache>>,
    /// Tab 标题（默认值，如 "Query 1"）
    pub(super) title: String,
    /// 上次执行的 SQL 摘要：成功执行后从 SQL 派生
    pub(super) short_title: Option<String>,
    /// 异步任务完成后挂这里的待推送 toast，下次 render 在 window 上推送
    pub(super) pending_notification: Option<Notification>,
    /// 上游显式指定的目标表 (schema, table)：表树点击触发的 SELECT 才有
    pub(super) pinned_target: Option<(String, String)>,
    /// 是否显示 SQL 编辑器
    pub(super) show_editor: bool,
    /// 分页状态：本次 run 命中"未手写 LIMIT 的单条 SELECT"时为 Some
    pub(super) pager: Option<paging::Pager>,
    /// 上次自动注入的 SQL（表树浏览 / 示例）。编辑器内容仍与之相等 = 用户未手改，
    /// 表树切表可安全原地覆盖；否则视为手写草稿，浏览须另开 Tab（防丢稿）
    pub(super) last_injected_sql: Option<String>,
    /// 编辑器变化订阅 keep-alive
    pub(super) _editor_sub: gpui::Subscription,
}

#[derive(Debug, Clone, Copy)]
pub enum QueryTabEvent {
    DraftChanged,
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        service: Arc<ConnectionService>,
        title: impl Into<String>,
        connection: Option<ConnectionConfig>,
        schema_cache: Arc<RwLock<SchemaCache>>,
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
            // SQL 补全：关键字 + 表名 + 列名（cache 共享）
            state.lsp.completion_provider = Some(
                crate::sql_completion::SqlCompletionProvider::new_rc(cache_for_provider),
            );
            state
        });
        let cache_for_result = schema_cache.clone();
        let result = cx.new(|cx| {
            let mut p = ResultPanel::new(window, cx);
            // 把执行器注入：单元格编辑弹框「确认修改」需要异步发 UPDATE
            p.set_executor(Some(service.clone()), connection.clone());
            // schema cache：判断 current_table 是否视图，从而禁用写按钮
            p.set_schema_cache(Some(cache_for_result));
            p
        });

        // 订阅编辑器内容变化：发现新提到的表 → 后台预拉它的列结构
        let editor_sub = cx.subscribe(&editor, |this: &mut Self, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                // 手改 SQL 即失去"表树单表数据"资格：清目标表，当前结果立即转只读
                // （程序注入走 set_value 不发 Change，不会误触）
                if this.pinned_target.is_some() && this.has_user_draft(cx) {
                    this.pinned_target = None;
                    this.result.update(cx, |r, cx| r.clear_editable_target(cx));
                }
                this.prefetch_columns_for_used_tables(cx);
                cx.emit(QueryTabEvent::DraftChanged);
            }
        });

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
            formatting: false,
            current_task: None,
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
        }
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
    pub fn draft_text(&self, cx: &gpui::App) -> Option<String> {
        self.has_user_draft(cx)
            .then(|| self.editor.read(cx).value().to_string())
    }

    /// 从本地偏好恢复手写草稿，不触发查询执行。
    pub fn restore_draft(
        &mut self,
        text: String,
        schema: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sql(text, window, cx);
        self.active_schema = schema.filter(|s| !s.is_empty());
        // 恢复内容必须被视为用户草稿，关闭时继续受保护。
        self.last_injected_sql = None;
    }

    /// 标记「当前编辑器内容是自动注入的」（表树浏览 / 示例），供草稿判定
    pub fn mark_injected(&mut self, sql: String) {
        self.last_injected_sql = Some(sql);
    }

    /// 工具条切换全局自动 LIMIT 档位；所有已打开查询标签立即使用同一值。
    pub(super) fn set_auto_limit(&mut self, limit: Option<usize>, cx: &mut Context<Self>) {
        ramag_ui::preferences::set_sql_auto_limit(limit, cx);
    }

    /// 由 QueryPanel 全局同步：是否展示顶部 SQL 编辑器
    pub fn set_show_editor(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.show_editor != v {
            self.show_editor = v;
            cx.notify();
        }
    }

    /// 切换表时调：清空结果集的列/行过滤框，避免旧过滤条件遮挡新表数据
    pub fn clear_result_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.result.update(cx, |r, cx| r.clear_filters(window, cx));
    }

    /// 上游设定/清除当前 Tab 的目标表（仅表树点击会注入；手动 run 不变）
    pub fn set_pinned_target(&mut self, target: Option<(String, String)>) {
        self.pinned_target = target;
    }

    /// 用于 TabBar 展示的标题：上次成功执行的 SQL 摘要 > 默认 Tab 名
    pub fn display_title(&self) -> &str {
        self.short_title.as_deref().unwrap_or(&self.title)
    }

    pub fn set_connection(&mut self, conn: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        // 切换连接时把默认库重置成新连接的 database 字段
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        self.connection = conn.clone();
        // 旧连接的分页状态不能带到新连接（base_sql 已不可信）
        self.pager = None;
        // 同步给 ResultPanel：单元格编辑弹框需要最新的连接来发 UPDATE
        let svc = self.service.clone();
        self.result.update(cx, |r, _| {
            r.set_executor(Some(svc), conn);
        });
        cx.notify();
    }

    /// 父级（ConnectionSession）同步当前活动库；点表树会调用
    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema != normalized {
            self.active_schema = normalized;
            cx.notify();
        }
    }

    /// 把 SQL 写入编辑器（替换原有内容）
    pub fn set_sql(
        &mut self,
        sql: impl Into<gpui::SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor
            .update(cx, |state, cx| state.set_value(sql, window, cx));
        // 用户改了 SQL 就清掉之前的 pinned_target：行内编辑不应再用旧目标表
        self.pinned_target = None;
        // 编辑器被整体替换后旧分页状态作废（避免"下一页"重跑已被换掉的 SQL）
        self.pager = None;
        // 默认视为普通写入（可能是历史填入等用户内容）；自动注入路径由调用方再 mark_injected
        self.last_injected_sql = None;
        // set_value 不发 InputEvent::Change（emit_events=false），手动触发预拉
        self.prefetch_columns_for_used_tables(cx);
        cx.notify();
    }

    /// 对外暴露：让其他视图（如点表树后）触发执行
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

    /// 把示例 SQL 写入编辑器：整体覆盖现有内容（与 MongoDB 行为一致，不保留旧语句）
    pub(super) fn insert_example(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |state, cx| {
            state.set_value(sql.to_string(), window, cx);
            state.focus(window, cx);
        });
        // 示例模板属自动注入：未手改前点表树仍可原地覆盖
        self.last_injected_sql = Some(sql.to_string());
        // set_value 不发 Change 事件，手动触发列结构预拉（与 set_sql 一致）
        self.prefetch_columns_for_used_tables(cx);
        cx.notify();
    }

    /// 聚焦编辑器（关闭 / 切换 Tab 后由 QueryPanel 调用，避免用户再点一下）
    pub fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }
}
