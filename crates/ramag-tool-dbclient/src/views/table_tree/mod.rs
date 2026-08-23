mod ddl;
mod load;
mod menus;
mod ops;
mod render;
mod row;
mod transfer_ops;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{AppContext as _, Context, EventEmitter, UniformListScrollHandle, Window};
use gpui_component::input::InputState;
use parking_lot::RwLock;
use ramag_app::ConnectionService;
use ramag_domain::entities::{Column, ConnectionConfig, DriverKind, ForeignKey, Index, Schema};
use ramag_ui::AsyncMutationGate;
use tracing::error;

use self::row::TreeRowsCacheEntry;
use crate::sql_completion::{SchemaCache, is_system_schema};

const MAX_LOADED_SCHEMA_TABLES: usize = 64;
const MAX_EXPANDED_TABLE_COLUMNS: usize = 32;

pub(crate) fn jsonl_import_description(schema: &str, table: &str) -> String {
    format!(
        "选择 JSONL 写入 {schema}.{table}。缺列使用默认值，多余键忽略；仅导入数据，覆盖会清空表。"
    )
}

pub struct TableTreePanel {
    pub(super) service: Arc<ConnectionService>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) loading_schemas: bool,
    pub(super) schemas: Vec<Schema>,
    pub(super) error: Option<String>,
    /// 表缓存与展开状态分离。
    pub(super) expanded: HashMap<String, SchemaTables>,
    pub(super) open_schemas: HashSet<String>,
    pub(super) full_search: Option<FullSearchProgress>,
    pub(super) full_search_generation: u64,
    /// 防止旧连接的异步结果回写。
    pub(super) metadata_generation: u64,
    pub(super) table_request_generation: u64,
    pub(super) column_request_generation: u64,
    pub(super) table_columns: HashMap<(String, String), TableColumns>,
    pub(super) selected: Option<(String, String)>,
    pub(super) show_system: bool,
    pub(super) search: gpui::Entity<InputState>,
    /// 缓存小写搜索词。
    pub(super) search_query: String,
    pub(super) schema_cache: Arc<RwLock<SchemaCache>>,
    pub(super) editor_visible: bool,
    pub(super) active_schema: Option<String>,
    pub(super) uniform_scroll: UniformListScrollHandle,
    tree_revision: u64,
    tree_rows_cache: RefCell<Option<TreeRowsCacheEntry>>,
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 请求渲染层移除常驻 DDL 提示。
    pub(super) clear_ddl_notification: bool,
    /// 防止旧 DDL 回包解锁新连接。
    pub(super) ddl_gate: AsyncMutationGate,
    pub(super) transfer: ramag_ui::TransferState,
    pub(super) _subscriptions: Vec<gpui::Subscription>,
}

#[derive(Default)]
pub(super) struct SchemaTables {
    pub(super) loading: bool,
    pub(super) tables: Vec<ramag_domain::entities::Table>,
    pub(super) error: Option<String>,
    pub(super) request_generation: u64,
}

#[derive(Clone, Copy)]
pub(super) struct FullSearchProgress {
    pub(super) completed: usize,
    pub(super) total: usize,
    pub(super) failed: usize,
    pub(super) generation: u64,
}

#[derive(Default)]
pub(super) struct TableColumns {
    pub(super) loading: bool,
    pub(super) columns: Vec<Column>,
    pub(super) indexes: Vec<Index>,
    pub(super) foreign_keys: Vec<ForeignKey>,
    pub(super) error: Option<String>,
    pub(super) request_generation: u64,
}

#[derive(Debug, Clone)]
pub enum TreeEvent {
    TableSelected {
        schema: String,
        table: String,
    },
    SchemaActivated {
        schema: String,
    },
    ShowCreateTable {
        schema: String,
        table: String,
        is_view: bool,
    },
    ShowSchemaDiagram {
        schema: String,
    },
    ModifyTable {
        schema: String,
        table: String,
    },
    ToggleSqlEditor,
}

impl EventEmitter<TreeEvent> for TableTreePanel {}

impl TableTreePanel {
    pub fn health(&self) -> (bool, bool) {
        (self.loading_schemas, self.error.is_some())
    }

    pub fn new(
        service: Arc<ConnectionService>,
        schema_cache: Arc<RwLock<SchemaCache>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ramag_ui::bounded_search_input(window, cx)
                .placeholder("搜索 schema / table")
                .clean_on_escape()
        });
        let subs = vec![
            // InputState::set_value 不触发 Change 事件。
            cx.observe(&search, |this: &mut Self, _, cx| {
                let query = this.search.read(cx).value().trim().to_lowercase();
                if query == this.search_query {
                    return;
                }
                this.search_query = query;
                // 非空搜索覆盖全库。
                this.ensure_search_coverage(cx);
                this.invalidate_tree_rows();
                cx.notify();
            }),
        ];

        Self {
            service,
            connection: None,
            loading_schemas: false,
            schemas: Vec::new(),
            error: None,
            expanded: HashMap::new(),
            open_schemas: HashSet::new(),
            full_search: None,
            full_search_generation: 0,
            metadata_generation: 0,
            table_request_generation: 0,
            column_request_generation: 0,
            table_columns: HashMap::new(),
            selected: None,
            show_system: false,
            search,
            search_query: String::new(),
            schema_cache,
            editor_visible: false,
            active_schema: None,
            uniform_scroll: UniformListScrollHandle::new(),
            tree_revision: 0,
            tree_rows_cache: RefCell::new(None),
            pending_notification: None,
            clear_ddl_notification: false,
            ddl_gate: AsyncMutationGate::default(),
            transfer: ramag_ui::TransferState::default(),
            _subscriptions: subs,
        }
    }

    pub fn set_editor_visible(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.editor_visible != v {
            self.editor_visible = v;
            cx.notify();
        }
    }

    pub(super) fn current_filter(&self, _cx: &gpui::App) -> String {
        self.search_query.clone()
    }

    pub(super) fn invalidate_tree_rows(&mut self) {
        self.tree_revision = self.tree_revision.wrapping_add(1);
        self.tree_rows_cache.get_mut().take();
    }

    pub(super) fn toggle_show_system(&mut self, cx: &mut Context<Self>) {
        self.show_system = !self.show_system;
        self.schema_cache.write().show_system = self.show_system;
        self.invalidate_tree_rows();
        cx.notify();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.connection.is_none() {
            return;
        }
        self.expanded.clear();
        self.open_schemas.clear();
        self.cancel_full_search(cx);
        self.table_columns.clear();
        self.selected = None;
        self.error = None;
        self.invalidate_tree_rows();
        self.load_schemas(cx);
    }

    /// 首次加载失败后重新激活会重试。
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.connection.is_some() && self.schemas.is_empty() && !self.loading_schemas {
            self.load_schemas(cx);
        }
    }

    pub fn set_connection(&mut self, conn: Option<ConnectionConfig>, cx: &mut Context<Self>) {
        self.ddl_gate.reset();
        self.connection = conn;
        self.schemas.clear();
        self.expanded.clear();
        self.open_schemas.clear();
        self.cancel_full_search(cx);
        self.table_columns.clear();
        self.selected = None;
        self.error = None;
        self.invalidate_tree_rows();
        if self.connection.is_some() {
            self.load_schemas(cx);
        } else {
            self.metadata_generation = self.metadata_generation.wrapping_add(1);
            self.loading_schemas = false;
            cx.notify();
        }
    }

    pub(super) fn load_schemas(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        self.metadata_generation = self.metadata_generation.wrapping_add(1);
        let metadata_generation = self.metadata_generation;
        self.loading_schemas = true;
        self.error = None;
        self.invalidate_tree_rows();
        cx.notify();

        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.list_schemas(&conn).await;
            if let Err(error) = &result {
                error!(
                    operation = "sql_metadata_schemas",
                    connection_id = %conn.id,
                    driver = ?conn.driver,
                    connection = %conn.name,
                    error = %error,
                    "load schemas failed"
                );
            }
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conn.id);
                if !is_current {
                    return;
                }
                this.loading_schemas = false;
                match result {
                    Ok(schemas) => {
                        let names: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();
                        this.schema_cache.write().all_schemas = names;
                        this.schemas = schemas;
                        this.invalidate_tree_rows();
                        if this.active_schema.is_none()
                            && let Some(default_name) = pick_default_schema(&conn, &this.schemas)
                        {
                            this.active_schema = Some(default_name.clone());
                            cx.emit(TreeEvent::SchemaActivated {
                                schema: default_name,
                            });
                        }
                    }
                    Err(e) => {
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// PG：`public` > 首个非系统；MySQL：config.database > 首个非系统；Redis：None
fn pick_default_schema(conn: &ConnectionConfig, schemas: &[Schema]) -> Option<String> {
    let first_user_schema = || {
        schemas
            .iter()
            .find(|s| !is_system_schema(&s.name))
            .map(|s| s.name.clone())
    };
    match conn.driver {
        DriverKind::Postgres => {
            if schemas.iter().any(|s| s.name == "public") {
                Some("public".to_string())
            } else {
                first_user_schema()
            }
        }
        DriverKind::Mysql => {
            if let Some(db) = conn.database.as_deref().filter(|s| !s.is_empty())
                && schemas.iter().any(|s| s.name == db)
            {
                Some(db.to_string())
            } else {
                first_user_schema()
            }
        }
        DriverKind::Redis | DriverKind::Mongodb => None,
    }
}
