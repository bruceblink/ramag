//! 多 Tab 查询面板：顶部 TabBar + 当前 QueryTab。仿 dbclient::QueryPanel 行为：
//! - 横向溢出滚动（tabs_scroll 句柄 + overflow_x_scroll）
//! - 新建 Tab 自动滚到末尾
//! - cmd-w 关当前 Tab；最后一个 Tab 关闭后 propagate 给全局 fallback 关窗
//! - 由 mongo_session 在 TreeEvent::CollectionSelected 时调 prefill_collection 自动开 Tab + 运行

mod drafts;

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
use ramag_ui::{
    CloseTab, MAX_EDITOR_TABS, can_open_editor_tab, icons,
    platform::{primary_shift_shortcut, primary_shortcut},
};

use crate::actions::{NewMongoQueryTab, ToggleMongoEditor};
use crate::views::query_tab::{MongoQueryTab, MongoQueryTabEvent};

/// 面板对外事件：Tab 内部请求经此上抛给 session 路由
#[derive(Debug, Clone)]
pub enum MongoQueryPanelEvent {
    /// 结果工具条发起的集合级 JSONL 导入（由集合树执行，进度显示在树侧）
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
    connection: Option<ConnectionConfig>,
    /// 当前默认 db（由 session 同步：连接配置 OR 树点击 db 行）
    database: String,
    tabs: Vec<Entity<MongoQueryTab>>,
    /// Tab 标题（与 tabs 一一对应；查询 N 自动编号，与 dbclient 一致）
    titles: Vec<String>,
    active: usize,
    /// Tab Bar 横向滚动句柄：tab 多到溢出时新建后滚到末尾
    tabs_scroll: ScrollHandle,
    /// 命令编辑器显隐（默认 false 隐藏；cmd-e 切换；新 Tab 跟随）
    show_editor: bool,
    /// 面板根焦点：隐藏编辑器后焦点收回这里，保证 cmd-e 仍能再次触发
    focus_handle: FocusHandle,
    /// 历史弹框事件订阅（单槽：重复打开整体替换，不随打开次数累积）
    history_sub: Option<Subscription>,
    /// 每个查询标签的草稿变化订阅，与 tabs 同下标。
    draft_subscriptions: Vec<Subscription>,
    /// 草稿落盘防抖代际；会话关闭后最后一次后台写仍可完成。
    draft_generation: Arc<std::sync::atomic::AtomicU64>,
    /// 草稿写入串行化；等待锁后再验代际，避免较慢旧写覆盖最新内容。
    draft_write_lock: Arc<futures::lock::Mutex<()>>,
    /// 读取旧草稿期间，空默认标签不得覆盖存储内容。
    draft_load_pending: bool,
    restoring_drafts: bool,
    /// 最近一次草稿落盘失败的原因：顶部常驻警示条展示，成功后自动清除
    pub(super) draft_persist_error: Option<String>,
}

impl MongoQueryPanel {
    pub fn new(service: Arc<MongoService>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            service,
            connection: None,
            database: "admin".to_string(),
            tabs: Vec::new(),
            titles: Vec::new(),
            active: 0,
            tabs_scroll: ScrollHandle::new(),
            // 隐藏编辑器，让结果区直接占满（与 dbclient 默认一致）
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

    /// 切换编辑器显隐，同步给所有 tab；返回当前可见状态。
    /// 显示→聚焦编辑器；隐藏→焦点收回面板根，保证 cmd-e 的 handler 仍在焦点链可反复触发
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
        if let Some(c) = &conn
            && let Some(db) = c.database.clone().filter(|s| !s.is_empty())
        {
            self.database = db;
        }
        self.connection = conn;
        // 重置 tabs（不同连接的 tabs 不共享上下文）
        self.tabs.clear();
        self.titles.clear();
        self.draft_subscriptions.clear();
        self.active = 0;
        self.load_persisted_drafts(window, cx);
        cx.notify();
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

    /// 树点 collection：复用当前激活 Tab（覆盖编辑器 + 运行）；如果还没 Tab 自动建一个。
    /// 活动 Tab 有手写草稿（非空且非上次自动注入）时不覆盖，另开 Tab 浏览（防丢稿）
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
        // 找出未使用的最小编号（与 dbclient::QueryPanel 同款策略）
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
        self.scroll_tabs_to_end();
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
        true
    }

    /// 「查询 N」自动编号：找最小未使用编号，关闭再新建会回收
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

    /// 聚焦当前激活 Tab 的编辑器；让 KeyContext 立即锁定到 MongoQueryTab，cmd-enter 等快捷键无需先点编辑器
    fn focus_active_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.update(cx, |t, cx| t.focus_editor(window, cx));
        }
    }

    /// 供 session 在 Tab 激活时聚焦：编辑器可见则聚焦编辑器（cmd-e 与 cmd-enter 都在焦点链上，
    /// 因 MongoQueryTab 嵌在 MongoQueryPanel 内）；隐藏则聚焦面板根，让 cmd-e 能唤出编辑器
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_editor {
            self.focus_active_editor(window, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    /// 大负 offset 让 tab bar 滚末尾；GPUI 自动 clamp 到 max_offset
    fn scroll_tabs_to_end(&self) {
        self.tabs_scroll
            .set_offset(Point::new(px(-99999.0), px(0.0)));
    }

    /// 打开查询历史弹框：搜索 / 复制 / 填入 / 重跑 / 删除 / 清空（与 SQL 历史中心同构）
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
                        // 复用示例插入语义：有手写草稿自动另开 Tab（防丢稿）
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
        // 防丢稿：有手写草稿先确认（确认弹窗模态，idx 在回调前不会漂移）
        let has_draft = self
            .tabs
            .get(idx)
            .is_some_and(|t| t.read(cx).has_user_draft(cx));
        if has_draft {
            let entity = cx.entity();
            ramag_ui::open_confirm(
                "关闭查询标签？",
                "该标签的编辑器有未保存的手写内容，关闭将丢弃。".to_string(),
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
            // 至少保留一个 Tab（与 dbclient 一致）
            self.add_tab(window, cx);
            return;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > idx {
            self.active -= 1;
        }
        self.focus_active_editor(window, cx);
        self.schedule_draft_persist(cx);
        cx.notify();
    }

    pub fn select_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx < self.tabs.len() && self.active != idx {
            self.draft_load_pending = false;
            self.active = idx;
            self.focus_active_editor(window, cx);
            self.schedule_draft_persist(cx);
            cx.notify();
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// 把示例命令写入当前激活 Tab 的编辑器（整体替换）；没 Tab 时先建一个。
    /// 有手写草稿时另开 Tab，不覆盖（防丢稿）
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

    /// 集合改名成功：同步受影响标签并让旧结果失效。
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

    /// 集合删除成功：清掉受影响标签的 DML 目标并标明结果失效。
    pub fn collection_dropped(&mut self, database: &str, collection: &str, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| {
                tab.collection_dropped(database, collection, cx)
            });
        }
        self.schedule_draft_persist(cx);
    }
}

impl Render for MongoQueryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let border = theme.border;

        // Tab Bar 元素列表（一会儿放进可滚动容器）；
        // 只有 1 个 Tab 时不渲染关闭按钮（与 dbclient::QueryPanel 一致：保证至少一个 Tab）
        let only_one = self.tabs.len() <= 1;
        let can_add_tab = can_open_editor_tab(self.tabs.len());
        let add_tab_disabled = self.connection.is_none() || !can_add_tab;
        let tab_items: Vec<gpui::AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, _tab)| {
                let title = self.titles.get(i).cloned().unwrap_or_default();
                let is_active = i == self.active;
                let row = h_flex()
                    .id(SharedString::from(format!("mongo-tab-{i}")))
                    .px(px(10.0))
                    .h(px(28.0))
                    .gap(px(6.0))
                    .flex_none()
                    .items_center()
                    .border_r_1()
                    .border_color(border)
                    .text_xs()
                    .when(is_active, |s| {
                        s.bg(theme.background)
                            .text_color(fg)
                            .border_b_1()
                            .border_color(theme.primary)
                    })
                    .when(!is_active, |s| s.text_color(muted))
                    .hover(|s| s.bg(theme.list_hover))
                    .cursor_pointer()
                    .child(SharedString::from(title))
                    .when(!only_one, |tab| {
                        tab.child(
                            ramag_ui::clickable_button(SharedString::from(format!(
                                "mongo-tab-close-{i}"
                            )))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.close_tab(i, window, cx);
                                },
                            )),
                        )
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.select_tab(i, window, cx)),
                    );
                row.into_any_element()
            })
            .collect();

        // 主体：当前 Tab 内容；没 Tab 时引导提示
        let body: gpui::AnyElement = if let Some(tab) = self.tabs.get(self.active) {
            tab.clone().into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .text_xs()
                .child(SharedString::from(
                    "（左侧选 collection 自动开 Tab，或点 + 新 Tab）",
                ))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .key_context("MongoQueryPanel")
            .on_action(
                cx.listener(|this, _: &NewMongoQueryTab, window, cx| {
                    this.add_tab(window, cx);
                }),
            )
            // 草稿落盘失败常驻警示：用户以为可跨重启恢复，静默失败等于丢稿
            .when_some(self.draft_persist_error.clone(), |panel, err| {
                let warning = theme.warning;
                let mut warn_bg = warning;
                warn_bg.a = 0.12;
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py(px(5.0))
                        .bg(warn_bg)
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(warning)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(format!(
                                    "⚠ 草稿自动保存失败：{err}（草稿可能无法跨重启恢复，请复制重要内容备份）"
                                ))),
                        )
                        .child(
                            ramag_ui::clickable_button("mongo-draft-persist-retry")
                                .ghost()
                                .small()
                                .label("重试")
                                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                                    this.schedule_draft_persist(cx);
                                })),
                        ),
                )
            })
            // 始终关闭当前查询标签；最后一个关闭后会立即补一个空白标签。
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.tab_count() > 0 {
                    let i = this.active;
                    this.close_tab(i, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            // cmd-e 切换编辑器显隐
            .on_action(cx.listener(|this, _: &ToggleMongoEditor, window, cx| {
                this.toggle_editor(window, cx);
            }))
            // Tab Bar 仅在 show_editor=true 时渲染（与 dbclient::QueryPanel 同款）
            .when(self.show_editor, |panel| {
                panel.child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .h(px(32.0))
                        .items_center()
                        .border_b_1()
                        .border_color(border)
                        .bg(theme.muted.opacity(0.10))
                        // 左：tabs 滚动区 + 「+」新建（跟在 tabs 之后，与 dbclient 一致）
                        .child(
                            h_flex()
                                .id("mongo-tabs-scroll")
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .items_center()
                                .overflow_x_scroll()
                                .track_scroll(&self.tabs_scroll)
                                .children(tab_items)
                                .child(
                                    ramag_ui::clickable_button("mongo-tab-add")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Plus)
                                        .tooltip(if self.connection.is_none() {
                                            "请先连接 MongoDB".to_string()
                                        } else if can_add_tab {
                                            format!("新建查询 ({})", primary_shortcut("T"))
                                        } else {
                                            format!("查询标签已达上限（{MAX_EDITOR_TABS} 个）")
                                        })
                                        .disabled(add_tab_disabled)
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.add_tab(window, cx);
                                            },
                                        )),
                                ),
                        )
                        // 右：示例 + 格式化（运行已移到结果区工具栏，与 dbclient 同位）
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .border_l_1()
                                .border_color(border)
                                .child(
                                    ramag_ui::clickable_button("mongo-history")
                                        .ghost()
                                        .small()
                                        .icon(IconName::Calendar)
                                        .tooltip("查询历史")
                                        .disabled(self.connection.is_none())
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.open_history_dialog(window, cx);
                                            },
                                        )),
                                )
                                .child({
                                    let entity = cx.entity();
                                    let coll = self
                                        .tabs
                                        .get(self.active)
                                        .and_then(|t| t.read(cx).collection.clone())
                                        .unwrap_or_default();
                                    ramag_ui::clickable_button("mongo-examples")
                                        .ghost()
                                        .small()
                                        .icon(icons::scroll_text())
                                        .tooltip("常用命令示例（有手写草稿时新建标签）")
                                        .pointer_dropdown_menu(move |menu, _, _| {
                                            let mut m = menu;
                                            for (label, cmd) in
                                                crate::views::examples::mongo_examples(&coll)
                                            {
                                                let e = entity.clone();
                                                m = m.item(ramag_ui::menu_item(label).on_click(
                                                    move |_, window, app| {
                                                        e.update(app, |panel, cx| {
                                                            panel.apply_example(&cmd, window, cx);
                                                        });
                                                    },
                                                ));
                                            }
                                            m
                                        })
                                })
                                .child(
                                    ramag_ui::clickable_button("mongo-format")
                                        .ghost()
                                        .small()
                                        .icon(icons::wand_sparkles())
                                        .tooltip(format!(
                                            "格式化 ({})",
                                            primary_shift_shortcut("F")
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                if let Some(tab) =
                                                    this.tabs.get(this.active).cloned()
                                                {
                                                    tab.update(cx, |t, cx| {
                                                        t.format_json(window, cx)
                                                    });
                                                }
                                            },
                                        )),
                                ),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}
