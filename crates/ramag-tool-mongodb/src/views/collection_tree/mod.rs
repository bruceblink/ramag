//! MongoDB 数据库与集合树。

mod ops;
mod row;
mod transfer_ops;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    Anchor, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};

use gpui_component::{
    ActiveTheme, Selectable as _, Sizable as _, button::ButtonVariants as _, h_flex,
    input::InputState, v_flex,
};
use ramag_app::MongoService;
use ramag_domain::entities::{ConnectionConfig, MongoCollection, MongoDatabase};
use ramag_ui::AsyncMutationGate;
use ramag_ui::PointerDropdownMenu as _;
use row::TreeRowsCacheEntry;
use tracing::{error, info};

const AUTO_LOAD_MAX_DATABASES: usize = 50;
const MAX_LOADED_DATABASES: usize = 64;
const MAX_LOADED_COLLECTION_BYTES: usize = ramag_domain::entities::MAX_METADATA_BYTES;

pub struct CollectionTreePanel {
    service: Arc<MongoService>,
    connection: Option<ConnectionConfig>,
    databases: Vec<MongoDatabase>,
    loading: bool,
    error: Option<String>,
    /// 集合缓存与展开状态分离，避免搜索改变展开项。
    expanded: HashMap<String, ExpandedState>,
    expanded_bytes: usize,
    open_databases: HashSet<String>,
    /// 旧连接的异步结果不得回写。
    metadata_generation: u64,
    collection_request_generation: u64,
    search_load_generation: u64,
    search_loading: bool,
    selected: Option<(String, String)>,
    active_db: Option<String>,
    search: Entity<InputState>,
    /// 小写搜索词；与输入实体分开缓存，避免焦点变化触发元数据补拉。
    search_query: String,
    show_system: bool,
    editor_visible: bool,
    uniform_scroll: UniformListScrollHandle,
    tree_revision: u64,
    tree_rows_cache: RefCell<Option<TreeRowsCacheEntry>>,
    auto_activate_pending: bool,
    pending_notification: Option<gpui_component::notification::Notification>,
    /// 连接切换会使旧写任务失效。
    mutation_gate: AsyncMutationGate,
    transfer: ramag_ui::TransferState,
    _subscriptions: Vec<Subscription>,
}

const SYSTEM_DBS: &[&str] = &["admin", "config", "local"];

pub(super) fn is_system_db(name: &str) -> bool {
    SYSTEM_DBS.contains(&name)
}

/// 优先使用连接配置，否则选择首个非系统库。
fn pick_default_db(conn: Option<&ConnectionConfig>, databases: &[MongoDatabase]) -> Option<String> {
    if let Some(db) = conn
        .and_then(|c| c.database.as_deref())
        .filter(|s| !s.is_empty())
        && databases.iter().any(|d| d.name == db)
    {
        return Some(db.to_string());
    }
    databases
        .iter()
        .find(|d| !is_system_db(&d.name))
        .map(|d| d.name.clone())
}

/// driver 已返回有序数据库；配置中的空库用二分定位插入，避免 UI 再排序最多五万项。
fn insert_configured_database(databases: &mut Vec<MongoDatabase>, configured: Option<String>) {
    let Some(name) = configured.filter(|name| !name.is_empty()) else {
        return;
    };
    if let Err(index) = databases.binary_search_by(|database| database.name.cmp(&name)) {
        databases.insert(
            index,
            MongoDatabase {
                name,
                size_on_disk: None,
                empty: true,
            },
        );
    }
}

#[derive(Default)]
struct ExpandedState {
    loading: bool,
    collections: Vec<MongoCollection>,
    retained_bytes: usize,
    error: Option<String>,
    request_generation: u64,
}

#[derive(Debug, Clone)]
pub enum TreeEvent {
    CollectionSelected {
        database: String,
        collection: String,
    },
    /// 集合改名成功，查询面板据此同步/失效旧上下文。
    CollectionRenamed {
        database: String,
        old: String,
        new: String,
    },
    /// 集合删除成功，查询面板据此禁用旧结果的编辑入口。
    CollectionDropped {
        database: String,
        collection: String,
    },
    /// 数据库删除成功，查询面板据此清除旧库与旧集合写入目标。
    DatabaseDropped {
        database: String,
    },
    DatabaseActivated {
        database: String,
    },
    ToggleEditor,
}

impl EventEmitter<TreeEvent> for CollectionTreePanel {}

impl CollectionTreePanel {
    pub fn new(service: Arc<MongoService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索 database / collection")
                .clean_on_escape()
        });
        let subs = vec![
            // InputState::set_value 不发 Change 事件，观察实体才能正确处理清除按钮。
            cx.observe(&search, |this: &mut Self, _, cx| {
                let query = this.search.read(cx).value().trim().to_lowercase();
                if query == this.search_query {
                    return;
                }
                this.search_query = query;
                // 非空搜索覆盖全库，而非仅过滤已展开节点。
                this.ensure_search_coverage(cx);
                this.invalidate_tree_rows();
                cx.notify();
            }),
        ];
        Self {
            service,
            connection: None,
            databases: Vec::new(),
            loading: false,
            error: None,
            expanded: HashMap::new(),
            expanded_bytes: 0,
            open_databases: HashSet::new(),
            metadata_generation: 0,
            collection_request_generation: 0,
            search_load_generation: 0,
            search_loading: false,
            selected: None,
            active_db: None,
            search,
            search_query: String::new(),
            show_system: false,
            editor_visible: false,
            uniform_scroll: UniformListScrollHandle::new(),
            tree_revision: 0,
            tree_rows_cache: RefCell::new(None),
            auto_activate_pending: false,
            pending_notification: None,
            mutation_gate: AsyncMutationGate::default(),
            transfer: ramag_ui::TransferState::default(),
            _subscriptions: subs,
        }
    }

    fn toggle_show_system(&mut self, cx: &mut Context<Self>) {
        self.show_system = !self.show_system;
        self.cancel_search_load();
        self.ensure_search_coverage(cx);
        self.invalidate_tree_rows();
        cx.notify();
    }

    fn invalidate_tree_rows(&mut self) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.tree_rows_cache.get_mut().take();
    }

    pub fn set_editor_visible(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.editor_visible != v {
            self.editor_visible = v;
            cx.notify();
        }
    }

    pub fn set_connection(&mut self, conn: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        self.mutation_gate.reset();
        self.cancel_search_load();
        self.active_db = conn
            .as_ref()
            .and_then(|c| c.database.clone())
            .filter(|s| !s.is_empty());
        self.connection = conn;
        self.databases.clear();
        self.expanded.clear();
        self.expanded_bytes = 0;
        self.open_databases.clear();
        self.selected = None;
        self.error = None;
        self.invalidate_tree_rows();
        self.auto_activate_pending = self.connection.is_some();
        if self.connection.is_some() {
            self.refresh_databases(cx);
        } else {
            self.metadata_generation = self.metadata_generation.wrapping_add(1);
            self.loading = false;
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_databases(cx);
        let expanded_dbs: Vec<String> = self.expanded.keys().cloned().collect();
        for db in expanded_dbs {
            self.load_collections(db, cx);
        }
    }

    pub fn health(&self) -> (bool, bool) {
        (self.loading, self.error.is_some())
    }

    /// 首次加载失败时，重新激活会重试且保留展开状态。
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.connection.is_some() && self.databases.is_empty() && !self.loading {
            self.refresh_databases(cx);
        }
    }

    fn refresh_databases(&mut self, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        self.cancel_search_load();
        self.metadata_generation = self.metadata_generation.wrapping_add(1);
        let metadata_generation = self.metadata_generation;
        let svc = self.service.clone();
        self.loading = true;
        self.error = None;
        self.invalidate_tree_rows();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = svc.list_databases(&conf).await;
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conf.id);
                if !is_current {
                    return;
                }
                this.loading = false;
                match r {
                    Ok(mut dbs) => {
                        info!(count = dbs.len(), "databases loaded");
                        // 空库可能不在服务端列表中，仍展示连接配置里的库。
                        insert_configured_database(&mut dbs, conf.database.clone());
                        this.databases = dbs;
                        if this.auto_activate_pending {
                            this.auto_activate_pending = false;
                            if let Some(default_db) =
                                pick_default_db(this.connection.as_ref(), &this.databases)
                            {
                                this.active_db = Some(default_db.clone());
                                cx.emit(TreeEvent::DatabaseActivated {
                                    database: default_db,
                                });
                            }
                        }
                        this.ensure_search_coverage(cx);
                    }
                    Err(e) => {
                        error!(error = %e, "load databases failed");
                        this.error = Some(e.to_string());
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }

    /// 顺序补拉限制并发；清空搜索会取消并回收临时缓存。
    fn ensure_search_coverage(&mut self, cx: &mut Context<Self>) {
        if self.search.read(cx).value().trim().is_empty() {
            self.cancel_search_load();
            let removed: Vec<String> = self
                .expanded
                .keys()
                .filter(|database| !self.open_databases.contains(*database))
                .cloned()
                .collect();
            for database in &removed {
                self.remove_expanded_entry(database);
            }
            if !removed.is_empty() {
                self.invalidate_tree_rows();
            }
            return;
        }
        if self.search_loading {
            return;
        }
        let searchable_databases = self
            .databases
            .iter()
            .filter(|database| self.show_system || !is_system_db(&database.name))
            .count();
        if searchable_databases > AUTO_LOAD_MAX_DATABASES {
            return;
        }
        let missing: Vec<String> = self
            .databases
            .iter()
            .filter(|database| self.show_system || !is_system_db(&database.name))
            .map(|d| d.name.clone())
            .filter(|name| {
                self.expanded
                    .get(name)
                    .is_none_or(|state| state.error.is_some())
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        let new_entries = missing
            .iter()
            .filter(|database| !self.expanded.contains_key(*database))
            .count();
        if self.expanded.len().saturating_add(new_entries) > MAX_LOADED_DATABASES {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "搜索最多加载 {MAX_LOADED_DATABASES} 个数据库；请先收起不再使用的数据库"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }

        self.search_load_generation = self.search_load_generation.wrapping_add(1);
        if self.search_load_generation == 0 {
            self.search_load_generation = 1;
        }
        let search_generation = self.search_load_generation;
        let metadata_generation = self.metadata_generation;
        let Some(conf) = self.connection.clone() else {
            return;
        };
        self.search_loading = true;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            for database in missing {
                let request_generation = this
                    .update(cx, |this, cx| {
                        let search_is_current = this.search_loading
                            && this.search_load_generation == search_generation
                            && this.metadata_generation == metadata_generation
                            && this.connection.as_ref().map(|current| &current.id)
                                == Some(&conf.id);
                        if !search_is_current {
                            return None;
                        }
                        if this
                            .expanded
                            .get(&database)
                            .is_some_and(|state| state.error.is_none())
                        {
                            return Some(0);
                        }
                        let request_generation = this.next_collection_request_generation();
                        let state = this.expanded.entry(database.clone()).or_default();
                        state.loading = true;
                        state.error = None;
                        state.request_generation = request_generation;
                        this.invalidate_tree_rows();
                        cx.notify();
                        Some(request_generation)
                    })
                    .ok()
                    .flatten();
                let Some(request_generation) = request_generation else {
                    return;
                };
                if request_generation == 0 {
                    continue;
                }

                let result = service.list_collections(&conf, &database).await;
                let should_continue = this
                    .update(cx, |this, cx| {
                        let search_is_current = this.search_loading
                            && this.search_load_generation == search_generation
                            && this.metadata_generation == metadata_generation
                            && this.connection.as_ref().map(|current| &current.id)
                                == Some(&conf.id);
                        if !search_is_current {
                            return false;
                        }
                        if !this.expanded.get(&database).is_some_and(|state| {
                            state.request_generation == request_generation
                        }) {
                            return true;
                        }
                        match result {
                            Ok(collections) => {
                                info!(db = %database, count = collections.len(), "search collections loaded");
                                if let Err(message) =
                                    this.store_collections(&database, collections, false)
                                    && let Some(state) = this.expanded.get_mut(&database)
                                {
                                    state.loading = false;
                                    state.error = Some(message);
                                }
                            }
                            Err(error) => {
                                error!(error = %error, db = %database, "load search collections failed");
                                if let Some(state) = this.expanded.get_mut(&database) {
                                    state.loading = false;
                                    state.error = Some(error.to_string());
                                }
                            }
                        }
                        this.invalidate_tree_rows();
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !should_continue {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.search_load_generation == search_generation {
                    this.search_loading = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn cancel_search_load(&mut self) {
        if self.search_loading {
            self.search_load_generation = self.search_load_generation.wrapping_add(1);
            if self.search_load_generation == 0 {
                self.search_load_generation = 1;
            }
            self.search_loading = false;
            let search_only_loading: Vec<String> = self
                .expanded
                .iter()
                .filter(|(database, state)| {
                    state.loading && !self.open_databases.contains(*database)
                })
                .map(|(database, _)| database.clone())
                .collect();
            for database in search_only_loading {
                self.remove_expanded_entry(&database);
            }
            for (database, state) in &mut self.expanded {
                if state.loading && self.open_databases.contains(database) {
                    state.loading = false;
                    state.error = Some("加载已取消，请重试".to_string());
                }
            }
        }
    }

    fn next_collection_request_generation(&mut self) -> u64 {
        self.collection_request_generation = self.collection_request_generation.wrapping_add(1);
        if self.collection_request_generation == 0 {
            self.collection_request_generation = 1;
        }
        self.collection_request_generation
    }

    pub(super) fn remove_expanded_entry(&mut self, database: &str) -> bool {
        let Some(removed) = self.expanded.remove(database) else {
            return false;
        };
        self.expanded_bytes = self.expanded_bytes.saturating_sub(removed.retained_bytes);
        true
    }

    fn store_collections(
        &mut self,
        database: &str,
        collections: Vec<MongoCollection>,
        allow_evict: bool,
    ) -> Result<(), String> {
        self.store_collections_with_limit(
            database,
            collections,
            allow_evict,
            MAX_LOADED_COLLECTION_BYTES,
        )
    }

    fn store_collections_with_limit(
        &mut self,
        database: &str,
        collections: Vec<MongoCollection>,
        allow_evict: bool,
        limit: usize,
    ) -> Result<(), String> {
        let retained_bytes =
            collection_list_retained_bytes(database, &collections, collections.capacity());
        if retained_bytes > limit {
            return Err(format!(
                "该数据库的集合元数据超过 {} MiB 缓存上限，请缩小数据库范围",
                limit / 1024 / 1024
            ));
        }
        let previous_bytes = self
            .expanded
            .get(database)
            .map_or(0, |state| state.retained_bytes);
        while allow_evict
            && prospective_collection_bytes(self.expanded_bytes, previous_bytes, retained_bytes)
                > limit
        {
            let evict = self
                .expanded
                .keys()
                .find(|cached| {
                    cached.as_str() != database
                        && !self.open_databases.contains(*cached)
                        && self.active_db.as_ref() != Some(*cached)
                })
                .cloned();
            let Some(evict) = evict else {
                break;
            };
            self.remove_expanded_entry(&evict);
        }
        if prospective_collection_bytes(self.expanded_bytes, previous_bytes, retained_bytes) > limit
        {
            return Err(format!(
                "已加载的集合元数据达到 {} MiB 上限，请清空搜索或收起不再使用的数据库后重试",
                limit / 1024 / 1024
            ));
        }
        let Some(state) = self.expanded.get_mut(database) else {
            return Err("集合列表请求已失效".to_string());
        };
        self.expanded_bytes = self
            .expanded_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(retained_bytes);
        state.collections = collections;
        state.retained_bytes = retained_bytes;
        state.loading = false;
        state.error = None;
        Ok(())
    }

    fn toggle_database(&mut self, db: &str, cx: &mut Context<Self>) {
        self.active_db = Some(db.to_string());
        if !self.open_databases.insert(db.to_string()) {
            self.open_databases.remove(db);
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        let needs_load = self
            .expanded
            .get(db)
            .is_none_or(|state| state.error.is_some());
        if needs_load {
            self.load_collections(db.to_string(), cx);
        } else {
            self.invalidate_tree_rows();
            cx.notify();
        }
        cx.emit(TreeEvent::DatabaseActivated {
            database: db.to_string(),
        });
    }

    fn load_collections(&mut self, db: String, cx: &mut Context<Self>) {
        let Some(conf) = self.connection.clone() else {
            return;
        };
        if !self.expanded.contains_key(&db) {
            while self.expanded.len() >= MAX_LOADED_DATABASES {
                let evict = self
                    .expanded
                    .keys()
                    .find(|database| {
                        !self.open_databases.contains(*database)
                            && self.active_db.as_ref() != Some(*database)
                    })
                    .cloned();
                let Some(evict) = evict else {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::warning(format!(
                            "最多同时保留 {MAX_LOADED_DATABASES} 个数据库的集合列表，请先收起不再使用的数据库"
                        ))
                        .autohide(true),
                    );
                    cx.notify();
                    return;
                };
                self.remove_expanded_entry(&evict);
            }
        }
        let request_generation = self.next_collection_request_generation();
        let state = self.expanded.entry(db.clone()).or_default();
        state.loading = true;
        state.error = None;
        state.request_generation = request_generation;
        self.invalidate_tree_rows();
        cx.notify();
        let svc = self.service.clone();
        let db_for_async = db.clone();
        let metadata_generation = self.metadata_generation;
        cx.spawn(async move |this, cx| {
            let r = svc.list_collections(&conf, &db_for_async).await;
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conf.id)
                    && this
                        .expanded
                        .get(&db_for_async)
                        .is_some_and(|state| state.request_generation == request_generation);
                if !is_current {
                    return;
                }
                match r {
                    Ok(cs) => {
                        info!(db = %db_for_async, count = cs.len(), "collections loaded");
                        if let Err(message) = this.store_collections(&db_for_async, cs, true)
                            && let Some(state) = this.expanded.get_mut(&db_for_async)
                        {
                            state.loading = false;
                            state.error = Some(message);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, db = %db_for_async, "load collections failed");
                        if let Some(state) = this.expanded.get_mut(&db_for_async) {
                            state.loading = false;
                            state.error = Some(e.to_string());
                        }
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }

    fn select_collection(&mut self, db: String, coll: String, cx: &mut Context<Self>) {
        self.active_db = Some(db.clone());
        self.selected = Some((db.clone(), coll.clone()));
        cx.emit(TreeEvent::CollectionSelected {
            database: db,
            collection: coll,
        });
        cx.notify();
    }

    fn select_database(&mut self, db: String, cx: &mut Context<Self>) {
        self.active_db = Some(db.clone());
        let opened = self.open_databases.insert(db.clone());
        let needs_load = self
            .expanded
            .get(&db)
            .is_none_or(|state| state.error.is_some());
        if needs_load {
            self.load_collections(db.clone(), cx);
        } else if opened {
            self.invalidate_tree_rows();
        }
        cx.emit(TreeEvent::DatabaseActivated { database: db });
        cx.notify();
    }

    fn current_filter(&self, _cx: &gpui::App) -> String {
        self.search_query.clone()
    }
}

fn collection_list_retained_bytes(
    database: &str,
    collections: &[MongoCollection],
    collection_capacity: usize,
) -> usize {
    collections.iter().fold(
        std::mem::size_of::<ExpandedState>()
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(database.len())
            .saturating_add(
                collection_capacity.saturating_mul(std::mem::size_of::<MongoCollection>()),
            ),
        |total, collection| {
            total
                .saturating_add(collection.name.capacity())
                .saturating_add(collection.database.capacity())
        },
    )
}

fn prospective_collection_bytes(current: usize, previous: usize, replacement: usize) -> usize {
    current.saturating_sub(previous).saturating_add(replacement)
}

impl Render for CollectionTreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(n) = self.pending_notification.take() {
            use gpui_component::WindowExt as _;
            window.push_notification(n, cx);
        }
        let theme = cx.theme();
        let muted_fg = theme.muted_foreground;
        let border = theme.border;
        let background = theme.background;

        let filter = self.current_filter(cx);

        let show_system = self.show_system;
        let active_label = self
            .active_db
            .clone()
            .unwrap_or_else(|| "未选库".to_string());
        let picker_label = format!("DB {active_label} ▾");
        let entity_for_picker = cx.entity().clone();
        let active_for_menu = self.active_db.clone();
        let picker_dbs: Vec<String> = self
            .databases
            .iter()
            .filter(|d| show_system || !is_system_db(&d.name))
            .map(|d| d.name.clone())
            .collect();
        let header = h_flex()
            .w_full()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .items_center()
            .gap(px(8.0))
            .child(
                ramag_ui::clickable_button("mongo-db-picker")
                    .ghost()
                    .small()
                    .label(picker_label)
                    .pointer_dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        let mut m = menu;
                        let active = active_for_menu.clone();
                        for d in &picker_dbs {
                            let d_owned = d.clone();
                            let is_active = active.as_deref() == Some(d.as_str());
                            let label = if is_active {
                                format!("✓ {d}")
                            } else {
                                format!("  {d}")
                            };
                            let entity = entity_for_picker.clone();
                            m = m.item(ramag_ui::menu_item(label).on_click(move |_, _, app| {
                                let d = d_owned.clone();
                                entity.update(app, |this, cx| this.select_database(d, cx));
                            }));
                        }
                        m
                    }),
            );

        let editor_visible = self.editor_visible;
        let search_row = h_flex()
            .w_full()
            .items_center()
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(border)
            .gap(px(6.0))
            .child(
                div().flex_1().min_w_0().child(
                    ramag_ui::cleanable_input(&self.search, "mongo-tree-search-clear", false, cx)
                        .small()
                        .prefix(
                            gpui_component::Icon::new(gpui_component::IconName::Search)
                                .small()
                                .text_color(muted_fg),
                        ),
                ),
            )
            .child(
                ramag_ui::clickable_button("toggle-system-dbs")
                    .ghost()
                    .xsmall()
                    .icon(if show_system {
                        gpui_component::IconName::Eye
                    } else {
                        gpui_component::IconName::EyeOff
                    })
                    .tooltip("系统库")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_show_system(cx))),
            )
            .child(
                ramag_ui::clickable_button("refresh-mongo-tree")
                    .ghost()
                    .xsmall()
                    .icon(ramag_ui::icons::refresh_cw())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                ramag_ui::clickable_button("toggle-mongo-editor")
                    .ghost()
                    .xsmall()
                    .icon(gpui_component::IconName::SquareTerminal)
                    .selected(editor_visible)
                    .tooltip("编辑器")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TreeEvent::ToggleEditor))),
            );

        let tree_view = self.tree_rows_view(&filter);
        let tree_rows = tree_view.rows;
        let body = uniform_list(
            "mongo-tree-rows",
            tree_rows.len(),
            cx.processor({
                let tree_rows = tree_rows.clone();
                move |this, range: Range<usize>, _w, cx| {
                    range
                        .map(|i| this.render_tree_row(&tree_rows[i], cx))
                        .collect::<Vec<_>>()
                }
            }),
        )
        .track_scroll(&self.uniform_scroll)
        .px(px(2.0))
        .py(px(4.0))
        .flex_1();

        let total_dbs = self.databases.len();
        let visible_dbs = tree_view.visible_databases;
        let mut footer_text = if total_dbs == visible_dbs {
            format!("数据库 ({total_dbs})")
        } else {
            format!("数据库 ({visible_dbs}/{total_dbs})")
        };
        let searchable_dbs = self
            .databases
            .iter()
            .filter(|database| show_system || !is_system_db(&database.name))
            .count();
        if !filter.is_empty() && searchable_dbs > AUTO_LOAD_MAX_DATABASES {
            footer_text.push_str(" · 库过多，搜索仅覆盖已展开的库");
        }
        if self.mutation_gate.is_busy() {
            footer_text.push_str(" · 写操作执行中…");
        }

        let transfer_row = ramag_ui::transfer_progress_row(
            "mongo-transfer-cancel",
            &self.transfer,
            |this: &mut Self| &this.transfer,
            cx,
        );

        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .child(header)
            .child(search_row)
            .children(transfer_row)
            .child(body)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_2()
                    .py(px(4.0))
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted_fg)
                    .child(SharedString::from(footer_text)),
            )
    }
}

#[cfg(test)]
mod cache_budget_tests {
    use super::*;

    #[test]
    fn collection_cache_estimate_counts_reserved_structs_and_strings() {
        let mut collections = Vec::with_capacity(8);
        collections.push(MongoCollection {
            name: "users".to_string(),
            database: "app".to_string(),
            is_view: false,
        });

        let bytes = collection_list_retained_bytes("app", &collections, collections.capacity());

        assert!(bytes >= 8 * std::mem::size_of::<MongoCollection>() + "users".len() + 2 * 3);
    }

    #[test]
    fn replacement_budget_subtracts_previous_entry_before_checking_limit() {
        assert_eq!(prospective_collection_bytes(100, 40, 60), 120);
        assert_eq!(prospective_collection_bytes(10, 20, 5), 5);
        assert_eq!(prospective_collection_bytes(usize::MAX, 0, 1), usize::MAX);
    }

    #[test]
    fn configured_database_is_inserted_once_without_resorting() {
        let mut databases = vec![
            MongoDatabase {
                name: "admin".into(),
                size_on_disk: None,
                empty: false,
            },
            MongoDatabase {
                name: "users".into(),
                size_on_disk: None,
                empty: false,
            },
        ];

        insert_configured_database(&mut databases, Some("app".into()));
        insert_configured_database(&mut databases, Some("users".into()));

        assert_eq!(
            databases
                .iter()
                .map(|database| database.name.as_str())
                .collect::<Vec<_>>(),
            ["admin", "app", "users"]
        );
        assert!(databases[1].empty);
    }
}
