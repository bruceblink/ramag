//! 连接管理页：行点击=打开（emit Selected），行内按钮独立 emit。
//! 搜索按 名称 / host / 用户名 / 数据库 不区分大小写子串匹配

mod render;
mod row;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// 版本探测状态。底层 Tokio 请求可能在界面任务结束后继续，因此取消只做标记；
    /// 请求结束后会再次清池，确保旧任务不能把资源建回来。
    version_requests: HashMap<ConnectionId, VersionRequest>,
    refresh_generation: u64,
    _subscriptions: Vec<gpui::Subscription>,
}

struct FilteredIndicesCacheEntry {
    connections: Arc<Vec<ConnectionConfig>>,
    query_lower: String,
    indices: Arc<Vec<usize>>,
}

struct VersionRequest {
    config: ConnectionConfig,
    cancelled: Arc<AtomicBool>,
    /// 配置变化期间若用户已重新连接，旧请求清理完成后再串行探测新配置。
    restart_config: Option<ConnectionConfig>,
}

impl Drop for VersionRequest {
    fn drop(&mut self) {
        // 面板销毁时 detached 请求仍可能在 Tokio 中运行；完成后必须走清理分支。
        self.cancelled.store(true, Ordering::Release);
    }
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
        let search =
            cx.new(|cx| ramag_ui::bounded_search_input(window, cx).placeholder("搜索连接"));

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
            version_requests: HashMap::new(),
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
    pub fn prefetch_version(&mut self, conn: &ConnectionConfig, cx: &mut Context<Self>) {
        if self.versions.contains_key(&conn.id) {
            return;
        }
        if let Some(request) = self.version_requests.get_mut(&conn.id) {
            if request.config == *conn {
                // 同配置快速关闭再重开：保留原探测，不重复握手。
                request.cancelled.store(false, Ordering::Release);
                request.restart_config = None;
            } else {
                // 新旧配置不能并发争用同一 ConnectionId 的池；旧请求清理后再启动新请求。
                request.cancelled.store(true, Ordering::Release);
                request.restart_config = Some(conn.clone());
            }
            return;
        }

        let conn = conn.clone();
        let mysql_svc = self.service.clone();
        let redis_svc = self.redis_service.clone();
        let mongo_svc = self.mongo_service.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.version_requests.insert(
            conn.id.clone(),
            VersionRequest {
                config: conn.clone(),
                cancelled: cancelled.clone(),
                restart_config: None,
            },
        );
        cx.spawn(async move |this, cx| {
            let result = match conn.driver {
                DriverKind::Mysql | DriverKind::Postgres => mysql_svc.server_version(&conn).await,
                DriverKind::Redis => redis_svc.server_version(&conn).await,
                DriverKind::Mongodb => mongo_svc.server_version(&conn).await,
            };
            let update_result = this.update(cx, |this, cx| {
                let Some(current) = this.version_requests.get(&conn.id) else {
                    return;
                };
                if !Arc::ptr_eq(&current.cancelled, &cancelled) {
                    return;
                }
                let was_cancelled = cancelled.load(Ordering::Acquire);
                let Some(request) = this.version_requests.remove(&conn.id) else {
                    return;
                };
                let restart = request.restart_config.clone();

                if was_cancelled {
                    evict_version_resources(&mysql_svc, &redis_svc, &mongo_svc, &conn.id);
                } else {
                    match result {
                        Ok(version) => {
                            this.versions.insert(conn.id.clone(), version);
                            cx.notify();
                        }
                        Err(error) => {
                            debug!(error = %error, conn = %conn.name, "fetch server version failed");
                        }
                    }
                }

                if let Some(next) = restart {
                    this.versions.remove(&next.id);
                    this.prefetch_version(&next, cx);
                }
            });
            if update_result.is_err() {
                // 面板已销毁：没有任何会话需要保留此次探测建立的资源。
                evict_version_resources(&mysql_svc, &redis_svc, &mongo_svc, &conn.id);
            }
        })
        .detach();
    }

    /// 关闭连接标签时标记探测不再需要；底层请求完成后会再次清池。
    pub fn cancel_version_prefetch(&mut self, id: &ConnectionId) {
        if let Some(request) = self.version_requests.get_mut(id) {
            request.cancelled.store(true, Ordering::Release);
            request.restart_config = None;
        }
    }

    /// 配置变化时版本文案也失效，并让旧探测进入“完成后清池”分支。
    pub fn invalidate_version(&mut self, id: &ConnectionId) {
        self.versions.remove(id);
        self.cancel_version_prefetch(id);
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

fn evict_version_resources(
    sql: &ConnectionService,
    redis: &RedisService,
    mongo: &MongoService,
    id: &ConnectionId,
) {
    sql.evict_all_pools(id);
    redis.evict_pool(id);
    mongo.evict_pool(id);
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

    #[test]
    fn dropping_version_request_marks_background_cleanup_required() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _request = VersionRequest {
                config: ConnectionConfig::new_mysql("local", "127.0.0.1", 3306, "root"),
                cancelled: cancelled.clone(),
                restart_config: None,
            };
        }

        assert!(cancelled.load(Ordering::Acquire));
    }
}
