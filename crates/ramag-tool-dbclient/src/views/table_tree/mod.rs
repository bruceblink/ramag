//! Schema 与数据表树。

mod ops;
mod render;
mod row;
mod transfer_ops;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{AppContext as _, Context, EventEmitter, UniformListScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState};
use parking_lot::RwLock;
use ramag_app::ConnectionService;
use ramag_domain::entities::{Column, ConnectionConfig, DriverKind, ForeignKey, Index, Schema};
use ramag_ui::AsyncMutationGate;
use tracing::error;

use self::row::TreeRowsCacheEntry;
use crate::sql_completion::{SchemaCache, is_system_schema};

const MAX_LOADED_SCHEMA_TABLES: usize = 64;
const MAX_EXPANDED_TABLE_COLUMNS: usize = 32;

/// 纯数据 JSONL 导入对话框文案；不会创建或修改表结构。
pub(crate) fn jsonl_import_description(schema: &str, table: &str) -> String {
    format!(
        "仅导入数据，不创建或修改表结构。选择冲突策略与 .jsonl 文件（可多选），\
         每行一个 JSON 对象，\
         按键名匹配 {schema}.{table} 的列插入；行内缺少的列走库默认值，\
         未匹配的键忽略。「跳过」冲突行跳过，「覆盖」先清空表\
         （不可恢复），「停止」遇冲突即报错。"
    )
}

pub struct TableTreePanel {
    pub(super) service: Arc<ConnectionService>,
    pub(super) connection: Option<ConnectionConfig>,
    pub(super) loading_schemas: bool,
    pub(super) schemas: Vec<Schema>,
    pub(super) error: Option<String>,
    /// 表缓存与展开状态分离，避免搜索改变展开项。
    pub(super) expanded: HashMap<String, SchemaTables>,
    pub(super) open_schemas: HashSet<String>,
    pub(super) full_search: Option<FullSearchProgress>,
    pub(super) full_search_generation: u64,
    /// 旧连接的异步结果不得回写。
    pub(super) metadata_generation: u64,
    pub(super) table_request_generation: u64,
    pub(super) column_request_generation: u64,
    pub(super) table_columns: HashMap<(String, String), TableColumns>,
    pub(super) selected: Option<(String, String)>,
    pub(super) show_system: bool,
    pub(super) search: gpui::Entity<InputState>,
    pub(super) schema_cache: Arc<RwLock<SchemaCache>>,
    pub(super) editor_visible: bool,
    pub(super) active_schema: Option<String>,
    pub(super) uniform_scroll: UniformListScrollHandle,
    tree_revision: u64,
    tree_rows_cache: RefCell<Option<TreeRowsCacheEntry>>,
    pub(super) pending_notification: Option<gpui_component::notification::Notification>,
    /// 旧连接的 DDL 回包不得解锁新连接。
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
            cx.subscribe(&search, |this: &mut Self, _, _e: &InputEvent, cx| {
                // 非空搜索覆盖全库，而非仅过滤已展开节点。
                this.ensure_search_coverage(cx);
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
            schema_cache,
            editor_visible: false,
            active_schema: None,
            uniform_scroll: UniformListScrollHandle::new(),
            tree_revision: 0,
            tree_rows_cache: RefCell::new(None),
            pending_notification: None,
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

    pub(super) fn current_filter(&self, cx: &gpui::App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
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

    /// 首次加载失败时，重新激活会重试且保留展开状态。
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
                        error!(error = %e, "load schemas failed");
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 搜索按需补拉；库过多时由用户显式触发，避免请求雪崩。
    fn ensure_search_coverage(&mut self, cx: &mut Context<Self>) {
        const AUTO_LOAD_MAX_SCHEMAS: usize = 50;
        if self.search.read(cx).value().trim().is_empty() {
            self.cancel_full_search(cx);
            let before = self.expanded.len();
            self.expanded
                .retain(|schema, _| self.open_schemas.contains(schema));
            self.table_columns
                .retain(|(schema, _), _| self.open_schemas.contains(schema));
            if self.expanded.len() != before {
                self.invalidate_tree_rows();
            }
            return;
        }
        let searchable_schemas = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .count();
        if searchable_schemas > AUTO_LOAD_MAX_SCHEMAS {
            return;
        }
        let missing: Vec<String> = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .map(|s| s.name.clone())
            .filter(|n| !self.expanded.contains_key(n))
            .collect();
        for name in missing {
            self.load_tables_for(name, cx);
        }
    }

    pub(super) fn toggle_schema(&mut self, schema_name: String, cx: &mut Context<Self>) {
        self.active_schema = Some(schema_name.clone());
        cx.emit(TreeEvent::SchemaActivated {
            schema: schema_name.clone(),
        });

        if !self.open_schemas.insert(schema_name.clone()) {
            self.open_schemas.remove(&schema_name);
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        let needs_load = self
            .expanded
            .get(&schema_name)
            .is_none_or(|entry| entry.error.is_some());
        if needs_load {
            self.load_tables_for(schema_name, cx);
        } else {
            self.invalidate_tree_rows();
            cx.notify();
        }
    }

    /// 顺序补拉限制并发；取消后丢弃过期结果。
    pub(super) fn load_all_tables_for_search(&mut self, cx: &mut Context<Self>) {
        if self.full_search.is_some() || self.search.read(cx).value().trim().is_empty() {
            return;
        }
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let missing: Vec<String> = self
            .schemas
            .iter()
            .filter(|schema| self.show_system || !is_system_schema(&schema.name))
            .map(|schema| schema.name.clone())
            .filter(|name| {
                self.expanded
                    .get(name)
                    .is_none_or(|entry| entry.error.is_some())
            })
            .collect();
        let new_entries = missing
            .iter()
            .filter(|schema| !self.expanded.contains_key(*schema))
            .count();
        if self.expanded.len().saturating_add(new_entries) > MAX_LOADED_SCHEMA_TABLES {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "全库搜索最多加载 {MAX_LOADED_SCHEMA_TABLES} 个 schema；请先选择具体 schema，或缩小数据库范围"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }
        if missing.is_empty() {
            return;
        }

        self.full_search_generation = self.full_search_generation.wrapping_add(1);
        let generation = self.full_search_generation;
        self.full_search = Some(FullSearchProgress {
            completed: 0,
            total: missing.len(),
            failed: 0,
            generation,
        });
        cx.notify();

        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            for schema in missing {
                let cache_generation = this
                    .update(cx, |this, _| {
                        this.schema_cache.write().begin_table_refresh(&schema)
                    })
                    .unwrap_or(0);
                if cache_generation == 0 {
                    break;
                }
                let result = service.list_tables(&conn, &schema).await;
                let should_continue = this
                    .update(cx, |this, cx| {
                        let is_current = this.connection.as_ref().map(|current| &current.id)
                            == Some(&conn.id)
                            && this
                                .full_search
                                .is_some_and(|progress| progress.generation == generation);
                        if !is_current {
                            this.schema_cache
                                .write()
                                .cancel_table_refresh(&schema, cache_generation);
                            return false;
                        }

                        let entry = this.expanded.entry(schema.clone()).or_default();
                        entry.loading = false;
                        match result {
                            Ok(tables) => {
                                let names = tables.iter().map(|table| table.name.clone()).collect();
                                let views = tables
                                    .iter()
                                    .filter(|table| table.is_view)
                                    .map(|table| table.name.clone())
                                    .collect();
                                this.schema_cache.write().finish_table_refresh(
                                    schema.clone(),
                                    cache_generation,
                                    names,
                                    views,
                                );
                                entry.tables = tables;
                                entry.error = None;
                            }
                            Err(err) => {
                                this.schema_cache
                                    .write()
                                    .cancel_table_refresh(&schema, cache_generation);
                                error!(error = %err, schema = %schema, "load full-search tables failed");
                                entry.error = Some(err.to_string());
                                if let Some(progress) = this.full_search.as_mut() {
                                    progress.failed += 1;
                                }
                            }
                        }

                        let mut done = false;
                        if let Some(progress) = this.full_search.as_mut() {
                            progress.completed += 1;
                            done = progress.completed == progress.total;
                        }
                        if done {
                            this.full_search = None;
                        }
                        this.invalidate_tree_rows();
                        cx.notify();
                        !done
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn cancel_full_search(&mut self, cx: &mut Context<Self>) {
        if self.full_search.take().is_some() {
            self.full_search_generation = self.full_search_generation.wrapping_add(1);
            cx.notify();
        }
    }

    pub(super) fn load_tables_for(&mut self, schema_name: String, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        if !self.expanded.contains_key(&schema_name) {
            while self.expanded.len() >= MAX_LOADED_SCHEMA_TABLES {
                let evict = self
                    .expanded
                    .keys()
                    .find(|schema| {
                        !self.open_schemas.contains(*schema)
                            && self.active_schema.as_ref() != Some(*schema)
                    })
                    .cloned();
                let Some(evict) = evict else {
                    self.pending_notification = Some(
                        gpui_component::notification::Notification::warning(format!(
                            "最多同时保留 {MAX_LOADED_SCHEMA_TABLES} 个 schema 的表列表，请先收起不再使用的 schema"
                        ))
                        .autohide(true),
                    );
                    cx.notify();
                    return;
                };
                self.expanded.remove(&evict);
                self.table_columns.retain(|(schema, _), _| schema != &evict);
            }
        }
        self.table_request_generation = self.table_request_generation.wrapping_add(1);
        let request_generation = self.table_request_generation;
        let entry = self.expanded.entry(schema_name.clone()).or_default();
        entry.loading = true;
        entry.error = None;
        entry.request_generation = request_generation;
        self.invalidate_tree_rows();
        cx.notify();

        let svc = self.service.clone();
        let schema_for_async = schema_name.clone();
        let metadata_generation = self.metadata_generation;
        let cache_generation = self
            .schema_cache
            .write()
            .begin_table_refresh(&schema_for_async);
        cx.spawn(async move |this, cx| {
            let result = svc.list_tables(&conn, &schema_for_async).await;
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conn.id)
                    && this
                        .expanded
                        .get(&schema_for_async)
                        .is_some_and(|entry| entry.request_generation == request_generation);
                if !is_current {
                    this.schema_cache
                        .write()
                        .cancel_table_refresh(&schema_for_async, cache_generation);
                    return;
                }
                match result {
                    Ok(tables) => {
                        let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
                        let view_set: std::collections::HashSet<String> = tables
                            .iter()
                            .filter(|t| t.is_view)
                            .map(|t| t.name.clone())
                            .collect();
                        this.schema_cache.write().finish_table_refresh(
                            schema_for_async.clone(),
                            cache_generation,
                            names,
                            view_set,
                        );
                        let Some(entry) = this.expanded.get_mut(&schema_for_async) else {
                            return;
                        };
                        entry.loading = false;
                        entry.tables = tables;
                        entry.error = None;
                    }
                    Err(e) => {
                        this.schema_cache
                            .write()
                            .cancel_table_refresh(&schema_for_async, cache_generation);
                        error!(error = %e, schema = %schema_for_async, "load tables failed");
                        let Some(entry) = this.expanded.get_mut(&schema_for_async) else {
                            return;
                        };
                        entry.loading = false;
                        entry.error = Some(e.to_string());
                    }
                }
                this.invalidate_tree_rows();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn handle_table_click(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some((schema.clone(), table.clone()));
        if self.active_schema.as_deref() != Some(schema.as_str()) {
            self.active_schema = Some(schema.clone());
            cx.emit(TreeEvent::SchemaActivated {
                schema: schema.clone(),
            });
        }
        cx.emit(TreeEvent::TableSelected { schema, table });
        cx.notify();
    }

    pub(super) fn handle_show_ddl(
        &mut self,
        schema: String,
        table: String,
        is_view: bool,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TreeEvent::ShowCreateTable {
            schema,
            table,
            is_view,
        });
    }

    pub(super) fn toggle_table_columns(
        &mut self,
        schema: String,
        table: String,
        cx: &mut Context<Self>,
    ) {
        let key = (schema.clone(), table.clone());
        if self.table_columns.remove(&key).is_some() {
            self.invalidate_tree_rows();
            cx.notify();
            return;
        }
        if self.table_columns.len() >= MAX_EXPANDED_TABLE_COLUMNS {
            self.pending_notification = Some(
                gpui_component::notification::Notification::warning(format!(
                    "最多同时展开 {MAX_EXPANDED_TABLE_COLUMNS} 个表的列结构，请先收起不再查看的表"
                ))
                .autohide(true),
            );
            cx.notify();
            return;
        }

        let Some(conn) = self.connection.clone() else {
            return;
        };

        self.column_request_generation = self.column_request_generation.wrapping_add(1);
        let request_generation = self.column_request_generation;
        self.table_columns.insert(
            key.clone(),
            TableColumns {
                loading: true,
                request_generation,
                ..Default::default()
            },
        );
        self.invalidate_tree_rows();
        cx.notify();

        let svc = self.service.clone();
        let schema_async = schema.clone();
        let table_async = table.clone();
        let metadata_generation = self.metadata_generation;
        cx.spawn(async move |this, cx| {
            // 索引或外键失败不阻塞列结构。
            let cols_fut = svc.list_columns(&conn, &schema_async, &table_async);
            let idx_fut = svc.list_indexes(&conn, &schema_async, &table_async);
            let fk_fut = svc.list_foreign_keys(&conn, &schema_async, &table_async);
            let (cols_res, idx_res, fk_res) = futures::join!(cols_fut, idx_fut, fk_fut);
            let _ = this.update(cx, |this, cx| {
                let is_current = this.metadata_generation == metadata_generation
                    && this.connection.as_ref().map(|current| &current.id) == Some(&conn.id)
                    && this.table_columns.get(&key).is_some_and(|entry| {
                        entry.request_generation == request_generation
                    });
                if !is_current {
                    return;
                }
                let Some(entry) = this.table_columns.get_mut(&key) else {
                    return;
                };
                entry.loading = false;
                match cols_res {
                    Ok(cols) => {
                        let col_names: Vec<String> =
                            cols.iter().map(|c| c.name.clone()).collect();
                        this.schema_cache.write().cache_columns(
                            (schema_async.clone(), table_async.clone()),
                            col_names,
                        );
                        entry.columns = cols;
                    }
                    Err(e) => {
                        error!(error = %e, schema = %schema_async, table = %table_async, "load columns failed");
                        entry.error = Some(e.to_string());
                    }
                }
                match idx_res {
                    Ok(ix) => entry.indexes = ix,
                    Err(e) => {
                        tracing::warn!(error = %e, schema = %schema_async, table = %table_async, "load indexes failed");
                    }
                }
                match fk_res {
                    Ok(fk) => entry.foreign_keys = fk,
                    Err(e) => {
                        tracing::warn!(error = %e, schema = %schema_async, table = %table_async, "load foreign keys failed");
                    }
                }
                this.invalidate_tree_rows();
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
