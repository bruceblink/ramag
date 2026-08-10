mod load;
mod ops;
mod render;
mod row;
mod transfer_ops;

use render::{collection_list_retained_bytes, prospective_collection_bytes};

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
                        info!(
                            operation = "mongo_metadata_databases",
                            connection_id = %conf.id,
                            count = dbs.len(),
                            "databases loaded"
                        );
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
                        error!(
                            operation = "mongo_metadata_databases",
                            connection_id = %conf.id,
                            error = %e,
                            "load databases failed"
                        );
                        this.error = Some(e.to_string());
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests;
