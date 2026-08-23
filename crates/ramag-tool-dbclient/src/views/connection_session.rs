use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, Entity, FocusHandle, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, WindowExt as _, h_flex,
    resizable::{ResizableState, h_resizable, resizable_panel},
};
use parking_lot::RwLock;
use ramag_app::ConnectionService;
use ramag_domain::entities::{ConnectionConfig, DriverKind};
use tracing::{info, warn};

use crate::sql_completion::{SchemaCache, is_system_schema};
use crate::views::query_panel::{QueryPanel, QueryPanelEvent};
use crate::views::schema_diagram::SchemaDiagramPanel;
use crate::views::table_tree::{TableTreePanel, TreeEvent};

/// 元数据缓存刷新间隔，用于同步外部结构变更。
const CACHE_TTL: Duration = Duration::from_secs(60);

const TREE_WIDTH_INITIAL: f32 = 280.0;
const TREE_WIDTH_MIN: f32 = 180.0;
const TREE_WIDTH_MAX: f32 = 600.0;

pub struct ConnectionSession {
    config: ConnectionConfig,
    tree: Entity<TableTreePanel>,
    queries: Entity<QueryPanel>,
    resize_state: Entity<ResizableState>,
    /// 隐藏编辑器后承接焦点，保证快捷键仍在焦点链中。
    focus_handle: FocusHandle,
    /// 持有补全缓存，查询标签通过 `Arc` 共享。
    _schema_cache: Arc<RwLock<SchemaCache>>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionSession {
    pub fn new(
        config: ConnectionConfig,
        service: Arc<ConnectionService>,
        result_memory: ramag_ui::ResultMemoryBudget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let schema_cache = SchemaCache::new_shared();
        schema_cache.write().default_schema = config.database.clone();

        let tree =
            cx.new(|cx| TableTreePanel::new(service.clone(), schema_cache.clone(), window, cx));
        let queries = cx.new(|cx| {
            QueryPanel::new(
                service.clone(),
                schema_cache.clone(),
                result_memory,
                window,
                cx,
            )
        });

        let conn_for_tree = config.clone();
        tree.update(cx, |t, cx| t.set_connection(Some(conn_for_tree), cx));
        let conn_for_q = config.clone();
        queries.update(cx, |q, cx| q.set_connection(Some(conn_for_q), window, cx));

        Self::warm_schema_cache(service.clone(), config.clone(), schema_cache.clone(), cx);
        Self::start_cache_ttl(service.clone(), config.clone(), schema_cache.clone(), cx);

        let mut subs = Vec::new();

        let queries_clone = queries.clone();
        let driver_kind = config.driver;
        let connection_id = config.id.clone();
        subs.push(cx.subscribe_in(
            &tree,
            window,
            move |this: &mut Self, _, e: &TreeEvent, window, cx| match e {
                TreeEvent::TableSelected { schema, table } => {
                    info!(
                        operation = "sql_table_query_open",
                        connection_id = %connection_id,
                        driver = ?driver_kind,
                        schema = %schema,
                        table = %table,
                        "table query opened"
                    );
                    queries_clone.update(cx, |q, cx| {
                        q.set_active_schema(Some(schema.clone()), cx);
                    });
                    // LIMIT 交给查询层注入，保留裸查询的分页资格。
                    let qschema = driver_kind.quote_identifier(schema);
                    let qtable = driver_kind.quote_identifier(table);
                    let sql = format!("SELECT * FROM {qschema}.{qtable};");
                    let target = Some((schema.clone(), table.clone()));
                    queries_clone.update(cx, |q, cx| {
                        q.prefill_active_sql_and_run_with_target(sql, target, window, cx)
                    });
                }
                TreeEvent::SchemaActivated { schema } => {
                    info!(
                        operation = "sql_schema_activate",
                        connection_id = %connection_id,
                        driver = ?driver_kind,
                        schema = %schema,
                        "schema activated"
                    );
                    queries_clone.update(cx, |q, cx| {
                        q.set_active_schema(Some(schema.clone()), cx);
                    });
                }
                TreeEvent::ShowCreateTable {
                    schema,
                    table,
                    is_view,
                } => {
                    info!(
                        operation = "sql_show_create",
                        connection_id = %connection_id,
                        driver = ?driver_kind,
                        schema = %schema,
                        table = %table,
                        is_view,
                        "show create"
                    );
                    let sql = ramag_domain::entities::build_ddl_query(
                        driver_kind,
                        schema,
                        table,
                        *is_view,
                    );
                    queries_clone.update(cx, |q, cx| {
                        q.open_in_new_tab_and_run(sql, window, cx);
                    });
                }
                TreeEvent::ShowSchemaDiagram { schema } => {
                    this.open_schema_diagram(schema.clone(), window, cx);
                }
                TreeEvent::ModifyTable { schema, table } => {
                    this.tree.update(cx, |tree, cx| {
                        tree.open_modify_table_dialog(schema.clone(), table.clone(), window, cx);
                    });
                }
                TreeEvent::ToggleSqlEditor => {
                    this.toggle_sql_editor(window, cx);
                }
            },
        ));

        let resize_state = cx.new(|_| ResizableState::default());
        let tree_for_import = tree.clone();
        subs.push(cx.subscribe(
            &queries,
            move |_this: &mut Self, _, e: &QueryPanelEvent, cx| match e {
                QueryPanelEvent::TableImportRequested {
                    schema,
                    table,
                    policy,
                    files,
                } => {
                    tree_for_import.update(cx, |tree, cx| {
                        tree.import_table_from_files(
                            schema.clone(),
                            table.clone(),
                            *policy,
                            files.clone(),
                            cx,
                        );
                    });
                }
            },
        ));

        let focus_handle = cx.focus_handle();

        Self {
            config,
            tree,
            queries,
            resize_state,
            focus_handle,
            _schema_cache: schema_cache,
            _subscriptions: subs,
        }
    }

    /// 切换编辑器并保持快捷键焦点链。
    fn toggle_sql_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.queries.update(cx, |q, cx| q.toggle_editor(cx));
        info!(
            operation = "sql_editor_toggle",
            connection_id = %self.config.id,
            driver = ?self.config.driver,
            visible,
            "toggle sql editor"
        );
        self.tree
            .update(cx, |t, cx| t.set_editor_visible(visible, cx));
        if visible {
            self.queries
                .update(cx, |q, cx| q.focus_active_editor(window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    fn warm_schema_cache(
        service: Arc<ConnectionService>,
        config: ConnectionConfig,
        cache: Arc<RwLock<SchemaCache>>,
        cx: &mut Context<Self>,
    ) {
        cx.background_spawn(async move {
            warm_once(&service, &config, &cache).await;
        })
        .detach();
    }

    /// 会话销毁后停止周期刷新。
    fn start_cache_ttl(
        service: Arc<ConnectionService>,
        config: ConnectionConfig,
        cache: Arc<RwLock<SchemaCache>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CACHE_TTL).await;
                if this.update(cx, |_, _| ()).is_err() {
                    break;
                }
                warm_once(&service, &config, &cache).await;
            }
        })
        .detach();
    }

    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }

    pub fn health(&self, cx: &gpui::App) -> (bool, bool) {
        self.tree.read(cx).health()
    }

    pub fn title(&self) -> &str {
        &self.config.name
    }

    pub fn kind_label(&self) -> &'static str {
        match self.config.driver {
            DriverKind::Mysql => "MySQL",
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::Redis => "Redis",
            DriverKind::Mongodb => "MongoDB",
        }
    }

    pub fn ensure_loaded(&self, cx: &mut Context<Self>) {
        self.tree.update(cx, |t, cx| t.ensure_loaded(cx));
    }

    pub fn set_result_active(&self, active: bool, cx: &mut Context<Self>) {
        self.queries
            .update(cx, |queries, cx| queries.set_session_active(active, cx));
    }

    /// 激活标签时把焦点放回当前可交互区域。
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.queries.read(cx).is_editor_visible() {
            self.queries
                .update(cx, |q, cx| q.focus_active_editor(window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    /// Opens a read-only relationship preview built from the current SQL metadata.
    fn open_schema_diagram(&mut self, schema: String, window: &mut Window, cx: &mut Context<Self>) {
        let service = self.tree.read(cx).service.clone();
        let panel = cx.new(|cx| {
            SchemaDiagramPanel::new(service, self.config.clone(), schema.clone(), window, cx)
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let panel = panel.clone();
            dialog
                .title(format!("Schema Diagram · {schema}"))
                .width(px(1240.0))
                .margin_top(px(32.0))
                .content(move |content, _, _| content.child(panel.clone()))
        });
    }
}

impl Render for ConnectionSession {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        h_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .on_action(
                cx.listener(|this, _: &crate::actions::ToggleSqlEditor, window, cx| {
                    this.toggle_sql_editor(window, cx);
                }),
            )
            .child(
                h_resizable("session-resize")
                    .with_state(&self.resize_state)
                    .child(
                        resizable_panel()
                            .size(px(TREE_WIDTH_INITIAL))
                            .size_range(px(TREE_WIDTH_MIN)..px(TREE_WIDTH_MAX))
                            .child(
                                div()
                                    .size_full()
                                    .border_r_1()
                                    .border_color(theme.border)
                                    .child(self.tree.clone()),
                            ),
                    )
                    .child(
                        resizable_panel()
                            .child(div().size_full().min_w_0().child(self.queries.clone())),
                    ),
            )
    }
}

async fn warm_once(
    service: &ConnectionService,
    config: &ConnectionConfig,
    cache: &Arc<RwLock<SchemaCache>>,
) {
    let target_schema = if let Some(db) = &config.database {
        Some(db.clone())
    } else {
        match service.list_schemas(config).await {
            Ok(schemas) => {
                let preferred = (config.driver == DriverKind::Postgres)
                    .then(|| {
                        schemas
                            .iter()
                            .find(|schema| schema.name == "public")
                            .map(|schema| schema.name.clone())
                    })
                    .flatten();
                preferred.or_else(|| {
                    schemas
                        .into_iter()
                        .find(|schema| !is_system_schema(&schema.name))
                        .map(|schema| schema.name)
                })
            }
            Err(e) => {
                warn!(
                    operation = "sql_metadata_cache_warmup",
                    connection_id = %config.id,
                    driver = ?config.driver,
                    stage = "schemas",
                    error = %e,
                    "metadata cache warmup failed"
                );
                return;
            }
        }
    };
    let Some(schema) = target_schema else {
        return;
    };
    let generation = cache.write().begin_table_refresh(&schema);
    match service.list_tables(config, &schema).await {
        Ok(tables) => {
            let names = tables.iter().map(|table| table.name.clone()).collect();
            let views = tables
                .into_iter()
                .filter(|table| table.is_view)
                .map(|table| table.name)
                .collect();
            let refreshed =
                cache
                    .write()
                    .finish_table_refresh(schema.clone(), generation, names, views);
            if !refreshed {
                warn!(
                    operation = "sql_metadata_cache_warmup",
                    connection_id = %config.id,
                    driver = ?config.driver,
                    schema = %schema,
                    reason = "superseded_or_budget",
                    "metadata cache warmup discarded"
                );
                return;
            }
        }
        Err(e) => {
            cache.write().cancel_table_refresh(&schema, generation);
            warn!(
                operation = "sql_metadata_cache_warmup",
                connection_id = %config.id,
                driver = ?config.driver,
                schema = %schema,
                stage = "tables",
                error = %e,
                "metadata cache warmup failed"
            );
        }
    }
    info!(
        operation = "sql_metadata_cache_warmup",
        connection_id = %config.id,
        driver = ?config.driver,
        schema = %schema,
        schemas = cache.read().tables.len(),
        "schema cache refreshed"
    );
}
