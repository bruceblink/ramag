//! 连接同步配置与元数据选择面板。目标固定为列表中点击的当前连接。

mod catalog;
mod object_sections;
mod render;

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::{DataSyncConfirmation, DataSyncObjectCatalog, DataSyncService, PreparedDataSync};
use ramag_domain::entities::{
    ConnectionConfig, DataSyncRequest, DataSyncScope, DataSyncTaskId, DriverKind, MongoSyncScope,
    RedisKeyMapping, RedisSyncScope, SqlSyncScope, SyncObjectMapping, SyncObjectSelection,
};
use ramag_domain::error::{DomainError, Result};

use self::catalog::{preferred_scope, prefix_suggestions, selected_source, visible_catalog_items};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisMode {
    Database,
    Prefix,
    Keys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelState {
    Editing,
    Preflighting,
    Ready,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CatalogState {
    AwaitingSource,
    LoadingScopes,
    LoadingObjects,
    Ready,
    Error(String),
}

struct MappingEditor {
    source: String,
    target: Entity<InputState>,
    _subscription: Subscription,
}

pub struct DataSyncDialog {
    service: Arc<DataSyncService>,
    target: ConnectionConfig,
    sources: Vec<ConnectionConfig>,
    source_index: Option<usize>,
    selected_objects: bool,
    redis_mode: RedisMode,
    source_scopes: Vec<String>,
    target_scopes: Vec<String>,
    source_scope: Option<String>,
    source_objects: Vec<String>,
    target_objects: Vec<String>,
    target_objects_scope: Option<String>,
    catalog_truncated: bool,
    catalog_generation: u64,
    catalog_state: CatalogState,
    mapping_editors: Vec<MappingEditor>,
    target_scope: Entity<InputState>,
    object_query: Entity<InputState>,
    source_prefix: Entity<InputState>,
    target_prefix: Entity<InputState>,
    state: PanelState,
    prepared: Option<PreparedDataSync>,
    _subscriptions: Vec<Subscription>,
}

impl DataSyncDialog {
    pub fn new(
        service: Arc<DataSyncService>,
        target: ConnectionConfig,
        all_connections: &[ConnectionConfig],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut sources: Vec<_> = all_connections
            .iter()
            .filter(|connection| connection.id != target.id && connection.driver == target.driver)
            .cloned()
            .collect();
        sources.sort_by(|left, right| {
            catalog::connection_label(left).cmp(&catalog::connection_label(right))
        });
        let target_default = target.database.clone().unwrap_or_default();
        let target_scope = input(window, cx, &target_default, "选择已有或输入新目标名称");
        let object_query = input(window, cx, "", "搜索源表 / Collection / Key");
        let source_prefix = input(window, cx, "", "选择或输入源 Key 前缀");
        let target_prefix = input(window, cx, "", "选择或输入目标 Key 前缀（可空）");
        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe(&target_scope, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.target_objects.clear();
                    this.target_objects_scope = None;
                    this.invalidate(cx);
                }
            }),
        );
        subscriptions.push(cx.subscribe(&object_query, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        for field in [&source_prefix, &target_prefix] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.invalidate(cx);
                }
            }));
        }
        Self {
            service,
            target,
            sources,
            source_index: None,
            selected_objects: false,
            redis_mode: RedisMode::Database,
            source_scopes: Vec::new(),
            target_scopes: Vec::new(),
            source_scope: None,
            source_objects: Vec::new(),
            target_objects: Vec::new(),
            target_objects_scope: None,
            catalog_truncated: false,
            catalog_generation: 0,
            catalog_state: CatalogState::AwaitingSource,
            mapping_editors: Vec::new(),
            target_scope,
            object_query,
            source_prefix,
            target_prefix,
            state: PanelState::Editing,
            prepared: None,
            _subscriptions: subscriptions,
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        if self.state != PanelState::Preflighting {
            self.state = PanelState::Editing;
            self.prepared = None;
            cx.notify();
        }
    }

    fn source(&self) -> Result<&ConnectionConfig> {
        selected_source(&self.sources, self.source_index).ok_or_else(|| {
            if self.source_index.is_some() {
                DomainError::InvalidConfig("所选源连接已不可用，请重新选择".into())
            } else {
                DomainError::InvalidConfig("请先明确选择一个源连接".into())
            }
        })
    }

    fn reload_catalog_scopes(&mut self, cx: &mut Context<Self>) {
        let source = match self.source() {
            Ok(source) => source.clone(),
            Err(error) => {
                self.catalog_state = CatalogState::Error(error.message().into());
                cx.notify();
                return;
            }
        };
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        let generation = self.catalog_generation;
        self.catalog_state = CatalogState::LoadingScopes;
        self.source_scopes.clear();
        self.target_scopes.clear();
        self.source_scope = None;
        self.source_objects.clear();
        self.target_objects.clear();
        self.target_objects_scope = None;
        self.mapping_editors.clear();
        self.catalog_truncated = false;
        self.invalidate(cx);
        let target = self.target.clone();
        let service = self.service.clone();
        cx.spawn(async move |this, async_cx| {
            let (source_result, target_result) = futures::join!(
                service.list_catalog_scopes(&source),
                service.list_catalog_scopes(&target)
            );
            let _ = this.update(async_cx, move |this, cx| {
                if this.catalog_generation != generation
                    || this.source().ok().map(|current| &current.id) != Some(&source.id)
                {
                    return;
                }
                let (source_scopes, target_scopes) = match (source_result, target_result) {
                    (Ok(source_scopes), Ok(target_scopes)) => (source_scopes, target_scopes),
                    (Err(error), _) => {
                        this.catalog_state = CatalogState::Error(format!(
                            "读取源端可选范围失败：{}",
                            error.message()
                        ));
                        cx.notify();
                        return;
                    }
                    (_, Err(error)) => {
                        this.catalog_state = CatalogState::Error(format!(
                            "读取目标端可选范围失败：{}",
                            error.message()
                        ));
                        cx.notify();
                        return;
                    }
                };
                this.source_scopes = source_scopes;
                this.target_scopes = target_scopes;
                this.source_scope = preferred_scope(&source, &this.source_scopes);
                if this.source_scope.is_none() {
                    this.catalog_state = CatalogState::Error("源连接中没有可同步范围".into());
                    cx.notify();
                    return;
                }
                this.reload_catalog_objects(cx);
            });
        })
        .detach();
    }

    fn reload_catalog_objects(&mut self, cx: &mut Context<Self>) {
        let source = match self.source() {
            Ok(source) => source.clone(),
            Err(error) => {
                self.catalog_state = CatalogState::Error(error.message().into());
                cx.notify();
                return;
            }
        };
        let Some(source_scope) = self.source_scope.clone() else {
            self.catalog_state = CatalogState::Error("请选择源范围".into());
            cx.notify();
            return;
        };
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        let generation = self.catalog_generation;
        self.source_objects.clear();
        self.target_objects.clear();
        self.target_objects_scope = None;
        self.mapping_editors.clear();
        self.catalog_truncated = false;
        self.invalidate(cx);

        if source.driver == DriverKind::Redis && self.redis_mode == RedisMode::Database {
            self.catalog_state = CatalogState::Ready;
            cx.notify();
            return;
        }

        self.catalog_state = CatalogState::LoadingObjects;
        let target = self.target.clone();
        let target_scope = value(&self.target_scope, cx);
        let target_exists = self
            .target_scopes
            .iter()
            .any(|scope| scope == &target_scope);
        let service = self.service.clone();
        cx.spawn(async move |this, async_cx| {
            let source_future = service.list_catalog_objects(&source, &source_scope);
            let target_future = async {
                if target_exists {
                    service.list_catalog_objects(&target, &target_scope).await
                } else {
                    Ok(DataSyncObjectCatalog {
                        names: Vec::new(),
                        truncated: false,
                    })
                }
            };
            let (source_result, target_result) = futures::join!(source_future, target_future);
            let _ = this.update(async_cx, move |this, cx| {
                let current_target_scope = value(&this.target_scope, cx);
                if this.catalog_generation != generation
                    || this.source().ok().map(|current| &current.id) != Some(&source.id)
                    || this.source_scope.as_deref() != Some(source_scope.as_str())
                    || current_target_scope != target_scope
                {
                    return;
                }
                match (source_result, target_result) {
                    (Ok(source_catalog), Ok(target_catalog)) => {
                        this.source_objects = source_catalog.names;
                        this.target_objects = target_catalog.names;
                        this.target_objects_scope = target_exists.then_some(target_scope);
                        this.catalog_truncated =
                            source_catalog.truncated || target_catalog.truncated;
                        this.catalog_state = CatalogState::Ready;
                    }
                    (Err(error), _) => {
                        this.catalog_state =
                            CatalogState::Error(format!("读取源端对象失败：{}", error.message()));
                    }
                    (_, Err(error)) => {
                        this.catalog_state =
                            CatalogState::Error(format!("读取目标端对象失败：{}", error.message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_source(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.sources.len() || self.source_index == Some(index) {
            return;
        }
        self.source_index = Some(index);
        self.reload_catalog_scopes(cx);
    }

    fn select_source_scope(&mut self, scope: String, cx: &mut Context<Self>) {
        if !self
            .source_scopes
            .iter()
            .any(|candidate| candidate == &scope)
            || self.source_scope.as_deref() == Some(scope.as_str())
        {
            return;
        }
        self.source_scope = Some(scope);
        self.reload_catalog_objects(cx);
    }

    fn select_target_scope(&mut self, scope: String, window: &mut Window, cx: &mut Context<Self>) {
        self.target_scope.update(cx, |input, cx| {
            input.set_value(&scope, window, cx);
        });
        self.reload_catalog_objects(cx);
    }

    fn set_redis_mode(&mut self, mode: RedisMode, cx: &mut Context<Self>) {
        if self.redis_mode == mode {
            return;
        }
        self.redis_mode = mode;
        self.reload_catalog_objects(cx);
    }

    fn add_mapping(&mut self, source: String, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .mapping_editors
            .iter()
            .any(|mapping| mapping.source == source)
        {
            return;
        }
        let target = input(window, cx, &source, "目标名称");
        let subscription = cx.subscribe(&target, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.invalidate(cx);
            }
        });
        self.mapping_editors.push(MappingEditor {
            source,
            target,
            _subscription: subscription,
        });
        self.mapping_editors
            .sort_by(|left, right| left.source.cmp(&right.source));
        self.invalidate(cx);
    }

    fn toggle_mapping(&mut self, source: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self
            .mapping_editors
            .iter()
            .position(|mapping| mapping.source == source)
        {
            self.mapping_editors.remove(index);
            self.invalidate(cx);
        } else if self.source_objects.iter().any(|object| object == &source) {
            self.add_mapping(source, window, cx);
        }
    }

    fn select_visible_mappings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (visible, _) = self.visible_source_objects(cx);
        for source in visible {
            self.add_mapping(source, window, cx);
        }
    }

    fn clear_mappings(&mut self, cx: &mut Context<Self>) {
        if !self.mapping_editors.is_empty() {
            self.mapping_editors.clear();
            self.invalidate(cx);
        }
    }

    fn visible_source_objects(&self, cx: &Context<Self>) -> (Vec<String>, usize) {
        visible_catalog_items(&self.source_objects, &value(&self.object_query, cx))
    }

    fn source_prefix_suggestions(&self) -> Vec<String> {
        prefix_suggestions(&self.source_objects)
    }

    fn target_prefix_suggestions(&self) -> Vec<String> {
        prefix_suggestions(&self.target_objects)
    }

    fn mapping_values(&self, cx: &Context<Self>) -> Result<Vec<SyncObjectMapping>> {
        if self.mapping_editors.is_empty() {
            return Err(DomainError::InvalidConfig("请至少选择一个源对象".into()));
        }
        self.mapping_editors
            .iter()
            .map(|mapping| {
                let target = value(&mapping.target, cx);
                if target.is_empty() {
                    return Err(DomainError::InvalidConfig(format!(
                        "{} 的目标名称不能为空",
                        mapping.source
                    )));
                }
                Ok(SyncObjectMapping {
                    source: mapping.source.clone(),
                    target,
                })
            })
            .collect()
    }

    fn object_selection(&self, cx: &Context<Self>) -> Result<SyncObjectSelection> {
        if self.selected_objects {
            Ok(SyncObjectSelection::Selected(self.mapping_values(cx)?))
        } else {
            Ok(SyncObjectSelection::All)
        }
    }

    fn build_request(&self, cx: &Context<Self>) -> Result<DataSyncRequest> {
        if self.catalog_state != CatalogState::Ready {
            return Err(DomainError::InvalidConfig("源数据目录尚未加载完成".into()));
        }
        let source = self.source()?;
        let source_scope = self
            .source_scope
            .clone()
            .ok_or_else(|| DomainError::InvalidConfig("请选择源范围".into()))?;
        let target_scope = value(&self.target_scope, cx);
        let scope = match self.target.driver {
            DriverKind::Mysql | DriverKind::Postgres => DataSyncScope::Sql(SqlSyncScope {
                source_namespace: source_scope,
                target_namespace: target_scope,
                tables: self.object_selection(cx)?,
            }),
            DriverKind::Mongodb => DataSyncScope::Mongo(MongoSyncScope {
                source_database: source_scope,
                target_database: target_scope,
                collections: self.object_selection(cx)?,
            }),
            DriverKind::Redis => {
                let source_db = parse_redis_db(&source_scope, "源 DB")?;
                let target_db = parse_redis_db(&target_scope, "目标 DB")?;
                DataSyncScope::Redis(match self.redis_mode {
                    RedisMode::Database => RedisSyncScope::Database {
                        source_db,
                        target_db,
                        target_prefix: value(&self.target_prefix, cx),
                    },
                    RedisMode::Prefix => RedisSyncScope::Prefix {
                        source_db,
                        target_db,
                        source_prefix: value(&self.source_prefix, cx),
                        target_prefix: value(&self.target_prefix, cx),
                    },
                    RedisMode::Keys => RedisSyncScope::Keys {
                        source_db,
                        target_db,
                        mappings: self
                            .mapping_values(cx)?
                            .into_iter()
                            .map(|mapping| RedisKeyMapping {
                                source: mapping.source,
                                target: mapping.target,
                            })
                            .collect(),
                    },
                })
            }
        };
        Ok(DataSyncRequest {
            task_id: DataSyncTaskId::new(),
            source_connection_id: source.id.clone(),
            target_connection_id: self.target.id.clone(),
            engine: self.target.driver,
            scope,
        })
    }

    fn preflight(&mut self, cx: &mut Context<Self>) {
        if self.state == PanelState::Preflighting {
            return;
        }
        let request = match self.build_request(cx) {
            Ok(request) => request,
            Err(error) => {
                self.state = PanelState::Error(error.message().to_string());
                cx.notify();
                return;
            }
        };
        self.state = PanelState::Preflighting;
        self.prepared = None;
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.preflight(request).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(prepared) => {
                        this.prepared = Some(prepared);
                        this.state = PanelState::Ready;
                    }
                    Err(error) => {
                        this.prepared = None;
                        this.state = PanelState::Error(error.message().to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prepared) = self.prepared.take() else {
            self.state = PanelState::Error("预检结果不存在，请重新预检".into());
            cx.notify();
            return;
        };
        let confirmation = if prepared.report().requires_second_confirmation {
            DataSyncConfirmation::ContinueWithExistingTargets
        } else {
            DataSyncConfirmation::CreateMissingTargets
        };
        match self.service.start(prepared, confirmation) {
            Ok(started) => {
                let service = self.service.clone();
                gpui_component::WindowExt::close_dialog(window, cx);
                cx.spawn(async move |_, _| service.execute(started).await)
                    .detach();
            }
            Err(error) => {
                self.state = PanelState::Error(error.message().to_string());
                cx.notify();
            }
        }
    }
}

fn input(
    window: &mut Window,
    cx: &mut Context<DataSyncDialog>,
    default: &str,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .validate(|text, _| text.len() <= 16 * 1024)
            .default_value(default)
            .placeholder(placeholder)
    })
}

fn value(input: &Entity<InputState>, cx: &Context<DataSyncDialog>) -> String {
    input.read(cx).value().trim().to_string()
}

fn parse_redis_db(value: &str, label: &str) -> Result<u8> {
    value
        .parse::<u8>()
        .map_err(|_| DomainError::InvalidConfig(format!("{label} 必须是 0-255 的整数")))
}
