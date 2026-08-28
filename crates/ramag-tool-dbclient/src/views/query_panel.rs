mod drafts;
mod history;

mod render;
use std::sync::Arc;

use std::path::PathBuf;

use gpui::{
    AnyView, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Point, Render, ScrollHandle, SharedString, Styled, Window, div, prelude::*, px,
};

use crate::actions::NewQueryTab;
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, notification::Notification, v_flex,
};
use parking_lot::RwLock;
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig};
use ramag_ui::PointerDropdownMenu as _;
use ramag_ui::{CloseTab, MAX_EDITOR_TABS, ResultMemoryBudget, can_open_editor_tab};

use crate::sql_completion::SchemaCache;
use crate::views::connection_list::ConnectionListPanel;
use crate::views::query_tab::{QueryTab, QueryTabEvent};

const MAX_CLOSED_QUERY_DRAFTS: usize = 10;

#[derive(Debug, Clone)]
pub enum QueryPanelEvent {
    LocateTableRequested {
        schema: String,
        table: String,
    },
    TableImportRequested {
        schema: String,
        table: String,
        policy: ConflictPolicy,
        files: Vec<PathBuf>,
    },
}

impl EventEmitter<QueryPanelEvent> for QueryPanel {}

pub struct QueryPanel {
    service: Arc<ConnectionService>,
    schema_cache: Arc<RwLock<SchemaCache>>,
    result_memory: ResultMemoryBudget,
    tabs: Vec<Entity<QueryTab>>,
    titles: Vec<String>,
    active: usize,
    session_active: bool,
    connection: Option<ConnectionConfig>,
    connection_list: Option<Entity<ConnectionListPanel>>,
    active_schema: Option<String>,
    show_editor: bool,
    tabs_scroll: ScrollHandle,
    history_sub: Option<gpui::Subscription>,
    draft_subscriptions: Vec<gpui::Subscription>,
    /// 草稿落盘防抖代际。
    draft_generation: Arc<std::sync::atomic::AtomicU64>,
    /// 串行草稿写入，防止旧内容覆盖新内容。
    draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 读取草稿时阻止默认标签覆盖。
    draft_load_pending: bool,
    /// 恢复草稿时抑制中间态落盘。
    restoring_drafts: bool,
    /// 最近草稿落盘错误，显示在顶部。
    pub(super) draft_persist_error: Option<String>,
    /// 当前连接下最近关闭的手写查询草稿，按后进先出恢复。
    closed_drafts: Vec<ClosedQueryDraft>,
}

#[derive(Clone, Debug)]
struct ClosedQueryDraft {
    title: String,
    text: SharedString,
    context: Option<String>,
}

impl QueryPanel {
    pub fn new(
        service: Arc<ConnectionService>,
        schema_cache: Arc<RwLock<SchemaCache>>,
        result_memory: ResultMemoryBudget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            service,
            schema_cache,
            result_memory,
            tabs: Vec::new(),
            titles: Vec::new(),
            active: 0,
            session_active: false,
            connection: None,
            connection_list: None,
            active_schema: None,
            show_editor: false,
            tabs_scroll: ScrollHandle::new(),
            history_sub: None,
            draft_subscriptions: Vec::new(),
            draft_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            draft_write_lock: Arc::new(futures::lock::Mutex::new(())),
            draft_load_pending: false,
            restoring_drafts: false,
            draft_persist_error: None,
            closed_drafts: Vec::new(),
        };
        this.add_tab(window, cx);
        this
    }

    pub fn set_connection(
        &mut self,
        conn: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connection.as_ref().map(|current| &current.id)
            != conn.as_ref().map(|current| &current.id)
        {
            self.closed_drafts.clear();
        }
        self.connection = conn.clone();
        self.active_schema = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_connection(conn.clone(), cx));
        }
        self.load_persisted_drafts(window, cx);
        cx.notify();
    }

    pub(super) fn forward_tab_event(&mut self, event: &QueryTabEvent, cx: &mut Context<Self>) {
        match event {
            QueryTabEvent::DraftChanged => self.schedule_draft_persist(cx),
            QueryTabEvent::LocateTableRequested { schema, table } => {
                cx.emit(QueryPanelEvent::LocateTableRequested {
                    schema: schema.clone(),
                    table: table.clone(),
                });
            }
            QueryTabEvent::TableImportRequested {
                schema,
                table,
                policy,
                files,
            } => cx.emit(QueryPanelEvent::TableImportRequested {
                schema: schema.clone(),
                table: table.clone(),
                policy: *policy,
                files: files.clone(),
            }),
        }
    }

    pub(crate) fn set_connection_list(
        &mut self,
        connection_list: Option<Entity<ConnectionListPanel>>,
        cx: &mut Context<Self>,
    ) {
        self.connection_list = connection_list.clone();
        for tab in &self.tabs {
            tab.update(cx, |tab, _| {
                tab.set_connection_list(connection_list.clone())
            });
        }
    }

    /// Invalidates every SQL tab before its owning connection session is discarded.
    /// Each tab keeps its draft text, but running requests lose their generation and
    /// best-effort server cancellation is issued by the tab itself.
    pub(crate) fn cancel_pending_queries(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| tab.invalidate_query_context(cx));
        }
    }

    pub fn set_active_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let normalized = schema.filter(|s| !s.is_empty());
        if self.active_schema == normalized {
            return;
        }
        self.active_schema = normalized.clone();
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_active_schema(normalized.clone(), cx));
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    /// 切换 SQL 编辑器并返回可见状态。
    pub fn toggle_editor(&mut self, cx: &mut Context<Self>) -> bool {
        self.show_editor = !self.show_editor;
        let v = self.show_editor;
        for tab in self.tabs.iter() {
            tab.update(cx, |t, cx| t.set_show_editor(v, cx));
        }
        self.schedule_draft_persist(cx);
        cx.notify();
        v
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !can_open_editor_tab(self.tabs.len()) {
            window.push_notification(
                Notification::warning(format!(
                    "查询标签已达上限（{MAX_EDITOR_TABS} 个），请先关闭不需要的标签"
                ))
                .autohide(true),
                cx,
            );
            return false;
        }
        self.draft_load_pending = false;
        // 使用未占用的最小编号。
        let title = {
            let mut n = 1usize;
            loop {
                let candidate = format!("查询 {n}");
                if !self.titles.iter().any(|t| t == &candidate) {
                    break candidate;
                }
                n += 1;
            }
        };
        let tab = self.build_tab(title.clone(), window, cx);
        let sub = cx.subscribe(&tab, |this: &mut Self, _, e: &QueryTabEvent, cx| {
            this.forward_tab_event(e, cx);
        });
        self.tabs.push(tab);
        self.titles.push(title);
        self.draft_subscriptions.push(sub);
        self.active = self.tabs.len() - 1;
        self.sync_result_activity(cx);
        self.focus_active_editor(window, cx);
        // 滚动至最后一个标签。
        self.tabs_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
        self.schedule_draft_persist(cx);
        cx.notify();
        true
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // 有手写草稿时先确认。
        let has_draft = self
            .tabs
            .get(index)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        let has_transaction = self
            .tabs
            .get(index)
            .is_some_and(|t| t.read(cx).has_open_transaction());
        if has_draft || has_transaction {
            let entity = cx.entity();
            let message = match (has_draft, has_transaction) {
                (true, true) => "未保存内容将丢失，打开的事务将回滚。".to_string(),
                (true, false) => "未保存内容将丢失。".to_string(),
                (false, true) => "打开的事务将回滚，未提交修改会丢失。".to_string(),
                (false, false) => String::new(),
            };
            ramag_ui::open_confirm(
                "关闭查询标签？",
                message,
                "关闭",
                true,
                move |window, app| {
                    entity.update(app, |this, cx| this.close_tab_inner(index, window, cx));
                },
                window,
                cx,
            );
            return;
        }
        self.close_tab_inner(index, window, cx);
    }

    fn close_tab_inner(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.draft_load_pending = false;
        self.remember_closed_draft(index, cx);
        // 先取消执行中的查询。
        if let Some(tab) = self.tabs.get(index) {
            tab.update(cx, |t, cx| {
                t.cancel_if_running(window, cx);
                t.rollback_transaction_detached(cx);
            });
        }
        self.tabs.remove(index);
        self.titles.remove(index);
        if index < self.draft_subscriptions.len() {
            let _ = self.draft_subscriptions.remove(index);
        }
        if self.tabs.is_empty() {
            // 保持至少一个标签。
            self.add_tab(window, cx);
            return;
        }
        self.active = active_index_after_close(self.active, index, self.tabs.len());
        self.sync_result_activity(cx);
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    /// 保存关闭标签中的用户草稿；结果数据、运行任务和事务不进入恢复栈。
    fn remember_closed_draft(&mut self, index: usize, cx: &Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let tab_state = tab.read(cx);
        let Some(text) = tab_state.draft_text(cx) else {
            return;
        };
        let title = self
            .titles
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("查询 {}", index + 1));
        push_closed_draft(
            &mut self.closed_drafts,
            ClosedQueryDraft {
                title,
                text,
                context: tab_state.active_schema.clone(),
            },
        );
    }

    /// 恢复最近关闭的草稿并让新标签成为当前活动标签。
    pub(super) fn reopen_last_closed_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !can_open_editor_tab(self.tabs.len()) {
            window.push_notification(
                Notification::warning(format!(
                    "查询标签已达上限（{MAX_EDITOR_TABS} 个），请先关闭不需要的标签"
                ))
                .autohide(true),
                cx,
            );
            return;
        }
        let Some(draft) = self.closed_drafts.pop() else {
            return;
        };
        self.draft_load_pending = false;
        let tab = self.build_tab(draft.title.clone(), window, cx);
        tab.update(cx, |tab, cx| {
            tab.restore_draft(draft.text, draft.context, window, cx);
        });
        let sub = cx.subscribe(&tab, |this: &mut Self, _, e: &QueryTabEvent, cx| {
            this.forward_tab_event(e, cx);
        });
        self.tabs.push(tab);
        self.titles.push(draft.title);
        self.draft_subscriptions.push(sub);
        self.active = self.tabs.len() - 1;
        self.sync_result_activity(cx);
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() && self.active != index {
            self.draft_load_pending = false;
            self.active = index;
            self.sync_result_activity(cx);
            self.focus_active_editor(window, cx);
            self.schedule_draft_persist(cx);
            cx.notify();
        }
    }

    pub fn set_session_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.session_active == active {
            return;
        }
        self.session_active = active;
        self.sync_result_activity(cx);
    }

    pub(super) fn sync_result_activity(&self, cx: &mut Context<Self>) {
        for (index, tab) in self.tabs.iter().enumerate() {
            tab.update(cx, |tab, cx| {
                tab.set_result_active(self.session_active && index == self.active, cx)
            });
        }
    }

    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| t.focus_editor(window, cx));
        }
    }

    pub fn is_editor_visible(&self) -> bool {
        self.show_editor
    }

    pub fn prefill_active_sql_and_run(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.cancel_if_running(window, cx);
                if !t.set_sql(sql.clone(), window, cx) {
                    return;
                }
                t.mark_injected(sql);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 运行 SQL 并注入精确目标表。
    pub fn prefill_active_sql_and_run_with_target(
        &mut self,
        sql: String,
        target: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        // 有手写草稿时新建标签，避免覆盖。
        let has_draft = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.cancel_if_running(window, cx);
                // set_sql 会清除 target，须先调用。
                if !t.set_sql(sql.clone(), window, cx) {
                    return;
                }
                t.mark_injected(sql);
                t.set_pinned_target(target, cx);
                // 表结构变化，清除列筛选。
                t.clear_result_column_filter(window, cx);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 在新标签执行 SQL。
    pub fn open_in_new_tab_and_run(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.add_tab(window, cx) {
            return;
        }
        self.prefill_active_sql_and_run(sql, window, cx);
    }

    fn active_has_draft(&self, cx: &Context<Self>) -> bool {
        self.tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx))
    }

    /// 写入示例 SQL；有草稿时新建标签。
    fn insert_example_into_active(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        if self.active_has_draft(cx) && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active).cloned() {
            tab.update(cx, |t, cx| t.insert_example(sql, window, cx));
        }
        self.schedule_draft_persist(cx);
    }

    /// 填入 SQL 并聚焦；有草稿时新建标签。
    fn fill_active_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_load_pending = false;
        let needs_new_tab = self.tabs.is_empty() || self.active_has_draft(cx);
        if needs_new_tab && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                if !t.set_sql(sql, window, cx) {
                    return;
                }
                t.focus_editor(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }
}

fn push_closed_draft(stack: &mut Vec<ClosedQueryDraft>, draft: ClosedQueryDraft) {
    if stack.len() >= MAX_CLOSED_QUERY_DRAFTS {
        stack.remove(0);
    }
    stack.push(draft);
}

fn theme_active_bg(_secondary: gpui::Hsla, accent: gpui::Hsla) -> gpui::Hsla {
    let mut a = accent;
    a.a = 0.15;
    a
}

fn active_index_after_close(active: usize, closed: usize, remaining: usize) -> usize {
    debug_assert!(remaining > 0);
    if active >= remaining {
        remaining - 1
    } else if active > closed {
        active - 1
    } else {
        active
    }
}

#[cfg(test)]
mod tests;
