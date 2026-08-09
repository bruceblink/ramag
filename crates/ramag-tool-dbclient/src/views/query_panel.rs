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
use crate::views::query_tab::{QueryTab, QueryTabEvent};

#[derive(Debug, Clone)]
pub enum QueryPanelEvent {
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
    active_schema: Option<String>,
    show_editor: bool,
    tabs_scroll: ScrollHandle,
    history_sub: Option<gpui::Subscription>,
    draft_subscriptions: Vec<gpui::Subscription>,
    /// 草稿落盘防抖代际；Arc 让会话关闭后最后一次写入仍可完成。
    draft_generation: Arc<std::sync::atomic::AtomicU64>,
    /// 同一连接的草稿写入串行化；等待锁后再验代际，保证最终落盘的一定是最新快照。
    draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 异步读取旧草稿期间，空默认标签不应抢先覆盖持久化内容。
    draft_load_pending: bool,
    /// 异步恢复期间抑制中间态落盘。
    restoring_drafts: bool,
    /// 最近一次草稿落盘失败的原因：顶部常驻警示条展示，成功后自动清除
    pub(super) draft_persist_error: Option<String>,
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
            active_schema: None,
            // 数据浏览 / 导出是主场景，写 SQL 走 cmd-e 或表树按钮唤出
            show_editor: false,
            tabs_scroll: ScrollHandle::new(),
            history_sub: None,
            draft_subscriptions: Vec::new(),
            draft_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            draft_write_lock: Arc::new(futures::lock::Mutex::new(())),
            draft_load_pending: false,
            restoring_drafts: false,
            draft_persist_error: None,
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

    /// 切换 SQL 编辑器显隐：所有 Tab 同步；返回切换后的可见状态供调用方更新 UI
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
        // 找出未使用的最小编号（这样关闭"查询 1"再新建会重新得到"查询 1"）
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
        let sub = cx.subscribe(&tab, |this: &mut Self, _, e: &QueryTabEvent, cx| match e {
            QueryTabEvent::DraftChanged => this.schedule_draft_persist(cx),
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
        });
        self.tabs.push(tab);
        self.titles.push(title);
        self.draft_subscriptions.push(sub);
        self.active = self.tabs.len() - 1;
        self.sync_result_activity(cx);
        self.focus_active_editor(window, cx);
        // 大负 offset 让 tab bar 滚末尾，GPUI 自动 clamp 到 max_offset
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
        // 防丢稿：有手写草稿先确认（确认弹窗模态，index 在回调前不会漂移）
        let has_draft = self
            .tabs
            .get(index)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft {
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "关闭查询标签？",
                "该标签的编辑器有未保存的手写内容，关闭将丢弃。".to_string(),
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
        // 执行中的查询先取消（含后端 KILL），避免关 Tab 后语句仍占用数据库
        if let Some(tab) = self.tabs.get(index) {
            tab.update(cx, |t, cx| t.cancel_if_running(window, cx));
        }
        self.tabs.remove(index);
        self.titles.remove(index);
        if index < self.draft_subscriptions.len() {
            let _ = self.draft_subscriptions.remove(index);
        }
        if self.tabs.is_empty() {
            self.add_tab(window, cx); // 总保持至少一个 Tab（add_tab 内部会 focus）
            return;
        }
        self.active = active_index_after_close(self.active, index, self.tabs.len());
        self.sync_result_activity(cx);
        // 关闭后让新 active tab 编辑器获得焦点，无需再点一下
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
                t.set_sql(sql.clone(), window, cx);
                t.mark_injected(sql);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 同 prefill_active_sql_and_run，额外注入精确目标表 (schema, table)
    /// 表树点击触发的 SELECT 用：避开反引号内带短横线被 SQL parser 吞的坑
    pub fn prefill_active_sql_and_run_with_target(
        &mut self,
        sql: String,
        target: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        // 草稿保护：活动 Tab 存在手写内容（非空且非上次自动注入）时不覆盖，
        // 另开 Tab 浏览；未手改的浏览 SQL / 示例则原地复用，连点切表不膨胀 Tab
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
                // set_sql 内会清 pinned_target，所以必须先 set_sql 再 set_pinned_target
                t.set_sql(sql.clone(), window, cx);
                t.mark_injected(sql);
                t.set_pinned_target(target);
                // 表的列结构会变化；内容搜索作为用户条件跨表保留。
                t.clear_result_column_filter(window, cx);
                t.run(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 新建一个 Tab 写入 SQL 并立即执行（用于 SHOW CREATE TABLE 等辅助查询，
    /// 不污染用户当前编辑的 Tab）
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

    /// 把示例 SQL 插入当前激活 Tab 的编辑器（Tab 栏「示例」下拉用）。
    /// 有手写草稿时另开 Tab，不覆盖
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

    /// 把 SQL 填入当前活动 Tab 的编辑器并聚焦（不执行）。
    /// 有手写草稿时另开 Tab，不覆盖；面板恒保持至少一个 Tab，空列表仅是兜底防御
    fn fill_active_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_load_pending = false;
        let needs_new_tab = self.tabs.is_empty() || self.active_has_draft(cx);
        if needs_new_tab && !self.add_tab(window, cx) {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| {
                t.set_sql(sql, window, cx);
                t.focus_editor(window, cx);
            });
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }
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
