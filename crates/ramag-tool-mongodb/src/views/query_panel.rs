mod drafts;
mod render;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, IntoElement, ParentElement, Point,
    Render, ScrollHandle, SharedString, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, notification::Notification, v_flex,
};
use ramag_app::MongoService;
use ramag_domain::entities::{ConflictPolicy, ConnectionConfig};
use ramag_ui::PointerDropdownMenu as _;
use ramag_ui::{CloseTab, MAX_EDITOR_TABS, ResultMemoryBudget, can_open_editor_tab, icons};

use crate::actions::{NewMongoQueryTab, ToggleMongoEditor};
use crate::views::query_tab::{MongoQueryTab, MongoQueryTabEvent};

#[derive(Debug, Clone)]
pub enum MongoQueryPanelEvent {
    CollectionImportRequested {
        db: String,
        collection: String,
        policy: ConflictPolicy,
        files: Vec<PathBuf>,
    },
}

impl EventEmitter<MongoQueryPanelEvent> for MongoQueryPanel {}

pub struct MongoQueryPanel {
    service: Arc<MongoService>,
    result_memory: ResultMemoryBudget,
    connection: Option<ConnectionConfig>,
    database: String,
    tabs: Vec<Entity<MongoQueryTab>>,
    titles: Vec<String>,
    active: usize,
    session_active: bool,
    tabs_scroll: ScrollHandle,
    show_editor: bool,
    /// 面板根焦点，隐藏编辑器后接收快捷键。
    focus_handle: FocusHandle,
    history_sub: Option<Subscription>,
    draft_subscriptions: Vec<Subscription>,
    /// 草稿落盘防抖代际。
    draft_generation: Arc<std::sync::atomic::AtomicU64>,
    /// 串行草稿写入，防止旧内容覆盖新内容。
    draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 读取草稿时阻止默认标签覆盖。
    draft_load_pending: bool,
    restoring_drafts: bool,
    /// 最近草稿落盘错误，显示在顶部。
    pub(super) draft_persist_error: Option<String>,
}

impl MongoQueryPanel {
    pub fn new(
        service: Arc<MongoService>,
        result_memory: ResultMemoryBudget,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            service,
            result_memory,
            connection: None,
            database: "admin".to_string(),
            tabs: Vec::new(),
            titles: Vec::new(),
            active: 0,
            session_active: false,
            tabs_scroll: ScrollHandle::new(),
            show_editor: false,
            focus_handle: cx.focus_handle(),
            history_sub: None,
            draft_subscriptions: Vec::new(),
            draft_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            draft_write_lock: Arc::new(futures::lock::Mutex::new(())),
            draft_load_pending: false,
            restoring_drafts: false,
            draft_persist_error: None,
        }
    }

    /// 切换编辑器并返回可见状态。
    pub fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.show_editor = !self.show_editor;
        for tab in &self.tabs {
            tab.update(cx, |t, cx| t.set_show_editor(self.show_editor, cx));
        }
        self.schedule_draft_persist(cx);
        if self.show_editor {
            self.focus_active_editor(window, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
        self.show_editor
    }

    pub fn set_connection(
        &mut self,
        conn: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_pending_queries(cx);
        if let Some(c) = &conn
            && let Some(db) = c.database.clone().filter(|s| !s.is_empty())
        {
            self.database = db;
        }
        self.connection = conn;
        // 不同连接不共享标签。
        self.tabs.clear();
        self.titles.clear();
        self.draft_subscriptions.clear();
        self.active = 0;
        self.load_persisted_drafts(window, cx);
        cx.notify();
    }

    /// Stops every MongoDB tab before its connection session is discarded.
    /// MongoDB has no reliable server-side kill handle here, so the tab generation
    /// guard prevents late client responses from reaching a replacement context.
    pub(crate) fn cancel_pending_queries(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| tab.cancel_if_running(cx));
        }
    }

    pub fn set_database(&mut self, db: String, cx: &mut Context<Self>) {
        if self.database != db {
            self.database = db.clone();
            for tab in &self.tabs {
                tab.update(cx, |t, cx| t.set_database(db.clone(), cx));
            }
            self.schedule_draft_persist(cx);
            cx.notify();
        }
    }

    /// 打开集合查询；有草稿时另开标签。
    pub fn prefill_collection(
        &mut self,
        database: String,
        collection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_load_pending = false;
        let has_draft = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        let needs_new_tab = self.tabs.is_empty() || has_draft;
        if needs_new_tab && !self.add_tab(window, cx) {
            return;
        }
        self.database = database.clone();
        let Some(tab) = self.tabs.get(self.active).cloned() else {
            return;
        };
        tab.update(cx, |t, cx| {
            t.prefill_for_collection(database, collection, window, cx);
            t.request_run(window, cx);
        });
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    pub fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(conf) = self.connection.clone() else {
            window.push_notification(
                Notification::warning("请先连接 MongoDB，再新建查询标签").autohide(true),
                cx,
            );
            return false;
        };
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
        let title = self.next_tab_title();
        let tab = self.build_tab(conf, window, cx);
        let sub = cx.subscribe(
            &tab,
            |this: &mut Self, _, e: &MongoQueryTabEvent, cx| match e {
                MongoQueryTabEvent::DraftChanged => this.schedule_draft_persist(cx),
                MongoQueryTabEvent::CollectionImportRequested {
                    db,
                    collection,
                    policy,
                    files,
                } => cx.emit(MongoQueryPanelEvent::CollectionImportRequested {
                    db: db.clone(),
                    collection: collection.clone(),
                    policy: *policy,
                    files: files.clone(),
                }),
            },
        );
        self.tabs.push(tab);
        self.titles.push(title);
        self.draft_subscriptions.push(sub);
        self.active = self.tabs.len() - 1;
        self.sync_result_activity(cx);
        self.scroll_tabs_to_end();
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
        true
    }

    fn next_tab_title(&self) -> String {
        let mut n = 1usize;
        loop {
            let candidate = format!("查询 {n}");
            if !self.titles.iter().any(|t| t == &candidate) {
                break candidate;
            }
            n += 1;
        }
    }

    fn focus_active_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| t.focus_editor(window, cx));
        }
    }

    /// 激活标签时恢复编辑器或面板焦点。
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_editor {
            self.focus_active_editor(window, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn scroll_tabs_to_end(&self) {
        self.tabs_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
    }

    fn open_history_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let list = cx.new(|cx| {
            crate::views::history_dialog::MongoHistoryList::new(
                self.service.clone(),
                conn.id.clone(),
                window,
                cx,
            )
        });
        self.history_sub = Some(cx.subscribe_in(
            &list,
            window,
            |this: &mut Self,
             _,
             e: &crate::views::history_dialog::MongoHistoryEvent,
             window,
             cx| {
                use crate::views::history_dialog::MongoHistoryEvent;
                this.history_sub = None;
                match e {
                    MongoHistoryEvent::FillEditor(cmd) => {
                        window.close_dialog(cx);
                        this.apply_example(cmd, window, cx);
                        this.mark_active_as_user_draft(cx);
                    }
                    MongoHistoryEvent::RunCommand(cmd) => {
                        window.close_dialog(cx);
                        this.apply_example(cmd, window, cx);
                        this.mark_active_as_user_draft(cx);
                        if let Some(tab) = this.tabs.get(this.active) {
                            tab.update(cx, |t, cx| t.request_run(window, cx));
                        }
                    }
                }
            },
        ));
        let title = SharedString::from(format!("查询历史 · {}", conn.name));
        let panel_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let list = list.clone();
            let panel_for_close = panel_for_close.clone();
            let panel_for_title_close = panel_for_close.clone();
            dialog
                .title(ramag_ui::closable_dialog_title(
                    "mongo-history-dialog-close",
                    title.clone(),
                    move |_, app| {
                        panel_for_title_close.update(app, |this, _| this.history_sub = None);
                    },
                ))
                .close_button(false)
                .on_close(move |_, _, app| {
                    panel_for_close.update(app, |this, _| this.history_sub = None);
                })
                .width(px(760.0))
                .content(move |content, _, _| content.child(list.clone()))
        });
    }

    pub fn close_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        // 有手写草稿时先确认。
        let has_draft = self
            .tabs
            .get(idx)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft {
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "关闭查询标签？",
                "未保存内容将丢失。".to_string(),
                "关闭",
                true,
                move |window, app| {
                    entity.update(app, |this, cx| this.close_tab_inner(idx, window, cx));
                },
                window,
                cx,
            );
            return;
        }
        self.close_tab_inner(idx, window, cx);
    }

    fn close_tab_inner(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.draft_load_pending = false;
        if let Some(tab) = self.tabs.get(idx) {
            tab.update(cx, |tab, cx| tab.cancel_if_running(cx));
        }
        self.tabs.remove(idx);
        if idx < self.titles.len() {
            self.titles.remove(idx);
        }
        if idx < self.draft_subscriptions.len() {
            let _ = self.draft_subscriptions.remove(idx);
        }
        if self.tabs.is_empty() {
            // 至少保留一个标签。
            self.add_tab(window, cx);
            return;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > idx {
            self.active -= 1;
        }
        self.sync_result_activity(cx);
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    pub fn select_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx < self.tabs.len() && self.active != idx {
            self.draft_load_pending = false;
            self.active = idx;
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

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// 写入示例命令；有草稿时新建标签。
    fn apply_example(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_load_pending = false;
        let has_draft = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        let needs_new_tab = self.tabs.is_empty() || has_draft;
        if needs_new_tab && !self.add_tab(window, cx) {
            return;
        }
        let Some(tab) = self.tabs.get(self.active).cloned() else {
            return;
        };
        tab.update(cx, |t, cx| t.set_command(cmd, window, cx));
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    fn mark_active_as_user_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |tab, _| tab.mark_user_draft());
        }
        self.schedule_draft_persist(cx);
    }

    /// 集合改名后同步受影响标签。
    pub fn collection_renamed(
        &mut self,
        database: &str,
        old: &str,
        new: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| {
                tab.collection_renamed(database, old, new, window, cx)
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 集合删除后使相关结果失效。
    pub fn collection_dropped(&mut self, database: &str, collection: &str, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| {
                tab.collection_dropped(database, collection, cx)
            });
        }
        self.schedule_draft_persist(cx);
    }

    /// 数据库删除后切换至 `admin`。
    pub fn database_dropped(&mut self, database: &str, cx: &mut Context<Self>) {
        if self.database != database {
            return;
        }
        self.database = "admin".to_string();
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| tab.database_dropped(database, cx));
        }
        self.schedule_draft_persist(cx);
        cx.notify();
    }
}
