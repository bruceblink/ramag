//! 连接管理页：行点击=打开（emit Selected），行内按钮独立 emit。
//! 搜索按 名称 / host / 用户名 / 数据库 不区分大小写子串匹配

mod render;
mod row;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Window};
use gpui_component::input::{InputEvent, InputState};
use ramag_app::{ConnectionService, MongoService, RedisService};
use ramag_domain::entities::{
    ConnectionConfig, ConnectionId, DriverKind, contains_case_insensitive,
};
use tracing::{debug, error};

pub struct ConnectionListPanel {
    pub(super) service: Arc<ConnectionService>,
    /// Redis 连接的 server_version 走 redis_service
    redis_service: Arc<RedisService>,
    /// MongoDB 连接的 server_version 走 mongo_service
    mongo_service: Arc<MongoService>,
    pub(super) connections: Arc<Vec<ConnectionConfig>>,
    filtered_indices_cache: RefCell<Option<FilteredIndicesCacheEntry>>,
    pub(super) selected: Option<ConnectionId>,
    pub(super) loading: bool,
    /// 加载失败信息：非空时顶部显示错误条，避免把"读取失败"伪装成"没有连接"
    pub(super) load_error: Option<String>,
    pub(super) search: Entity<InputState>,
    /// 小写的搜索关键字
    pub(super) query: String,
    /// 服务端版本缓存。失败连接不入缓存避免重试
    pub(super) versions: HashMap<ConnectionId, String>,
    version_loading: HashSet<ConnectionId>,
    refresh_generation: u64,
    _subscriptions: Vec<gpui::Subscription>,
}

struct FilteredIndicesCacheEntry {
    connections: Arc<Vec<ConnectionConfig>>,
    query_lower: String,
    indices: Arc<Vec<usize>>,
}

impl FilteredIndicesCacheEntry {
    fn get(
        &self,
        connections: &Arc<Vec<ConnectionConfig>>,
        query_lower: &str,
    ) -> Option<Arc<Vec<usize>>> {
        (Arc::ptr_eq(&self.connections, connections) && self.query_lower == query_lower)
            .then(|| self.indices.clone())
    }
}

#[derive(Debug, Clone)]
pub enum ListEvent {
    Selected(ConnectionConfig),
    RequestNew,
    RequestEdit(ConnectionConfig),
    RequestDelete(ConnectionId),
}

impl EventEmitter<ListEvent> for ConnectionListPanel {}

impl ConnectionListPanel {
    pub fn new(
        service: Arc<ConnectionService>,
        redis_service: Arc<RedisService>,
        mongo_service: Arc<MongoService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索连接"));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = this.search.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            },
        ));

        let mut this = Self {
            service,
            redis_service,
            mongo_service,
            connections: Arc::new(Vec::new()),
            filtered_indices_cache: RefCell::new(None),
            selected: None,
            loading: true,
            load_error: None,
            search,
            query: String::new(),
            versions: HashMap::new(),
            version_loading: HashSet::new(),
            refresh_generation: 0,
            _subscriptions: subs,
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        self.loading = true;
        cx.notify();
        let svc = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = svc.list().await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(list) => {
                        let current_ids: HashSet<ConnectionId> = list
                            .iter()
                            .map(|connection| connection.id.clone())
                            .collect();
                        this.versions.retain(|id, _| current_ids.contains(id));
                        this.version_loading.retain(|id| current_ids.contains(id));
                        this.connections = Arc::new(list);
                        this.filtered_indices_cache.get_mut().take();
                        this.load_error = None;
                    }
                    Err(e) => {
                        error!(error = %e, "list connections failed");
                        // 保留旧列表（若有）而非清空成假空态，并记错误供顶部提示
                        this.load_error = Some(format!("加载连接列表失败：{e}"));
                    }
                }
                cx.notify();
                // refresh 不批量探测版本，避免对未打开连接反复试连不可达主机；
                // 真正 open_session 时由外层调 prefetch_version
            });
        })
        .detach();
    }

    /// 已缓存则跳过；失败仅 debug
    pub fn prefetch_version(&mut self, id: &ConnectionId, cx: &mut Context<Self>) {
        if self.versions.contains_key(id) || !self.version_loading.insert(id.clone()) {
            return;
        }
        let Some(conn) = self.connections.iter().find(|c| &c.id == id).cloned() else {
            self.version_loading.remove(id);
            return;
        };
        let generation = self.refresh_generation;
        let mysql_svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        cx.spawn(async move |this, cx| {
            let result = match conn.driver {
                DriverKind::Mysql | DriverKind::Postgres => mysql_svc.server_version(&conn).await,
                DriverKind::Redis => redis_svc.server_version(&conn).await,
                DriverKind::Mongodb => mongo_svc.server_version(&conn).await,
            };
            let _ = this.update(cx, |this, cx| {
                this.version_loading.remove(&conn.id);
                if this.refresh_generation != generation
                    || !this.connections.iter().any(|current| current.id == conn.id)
                {
                    return;
                }
                match result {
                    Ok(v) => {
                        this.versions.insert(conn.id.clone(), v);
                        cx.notify();
                    }
                    Err(e) => {
                        debug!(error = %e, conn = %conn.name, "fetch server version failed");
                    }
                }
            });
        })
        .detach();
    }

    pub fn connections(&self) -> &[ConnectionConfig] {
        self.connections.as_slice()
    }

    pub(super) fn handle_click(&mut self, conn: ConnectionConfig, cx: &mut Context<Self>) {
        self.selected = Some(conn.id.clone());
        cx.emit(ListEvent::Selected(conn));
        cx.notify();
    }

    pub(super) fn filtered_indices(&self) -> Arc<Vec<usize>> {
        {
            let cache = self.filtered_indices_cache.borrow();
            if let Some(indices) = cache
                .as_ref()
                .and_then(|entry| entry.get(&self.connections, &self.query))
            {
                return indices;
            }
        }

        let q = &self.query;
        let indices: Arc<Vec<usize>> = Arc::new(
            self.connections
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    contains_case_insensitive(&c.name, q)
                        || contains_case_insensitive(&c.host, q)
                        || contains_case_insensitive(&c.username, q)
                        || c.database
                            .as_deref()
                            .is_some_and(|database| contains_case_insensitive(database, q))
                })
                .map(|(index, _)| index)
                .collect(),
        );
        self.filtered_indices_cache
            .replace(Some(FilteredIndicesCacheEntry {
                connections: self.connections.clone(),
                query_lower: self.query.clone(),
                indices: indices.clone(),
            }));
        indices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_indices_cache_requires_same_source_and_query() {
        let connections = Arc::new(Vec::new());
        let indices = Arc::new(Vec::new());
        let cache = FilteredIndicesCacheEntry {
            connections: connections.clone(),
            query_lower: "local".into(),
            indices: indices.clone(),
        };

        let cached = cache.get(&connections, "local");
        assert!(
            cached
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, &indices))
        );
        assert!(cache.get(&connections, "remote").is_none());
        assert!(cache.get(&Arc::new(Vec::new()), "local").is_none());
    }
}
