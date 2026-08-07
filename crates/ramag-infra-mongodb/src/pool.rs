//! MongoDB 客户端缓存：按 `ConnectionId` 缓存 `mongodb::Client`。
//! Client 内部自带连接池 + 自动重连 + 多路复用，clone 是 Arc 廉价复制；db 切换走命令而非新连接

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use mongodb::Client;
use mongodb::options::{ClientOptions, Credential, ServerAddress};
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};
use ramag_domain::error::{DomainError, Result};
use tracing::{debug, info, warn};

use crate::errors::map_mongo_error;

type BuildLock = Arc<tokio::sync::Mutex<()>>;
type BuildLocks = Arc<DashMap<(ConnectionId, u64), BuildLock>>;

#[derive(Clone, Default)]
pub struct PoolCache {
    clients: Arc<DashMap<ConnectionId, CachedClient>>,
    /// 建连串行化锁：首次打开同一连接时 prefetch_version 与 list_databases 会并发 miss，
    /// 各建一个 Client、各跑一轮 SDAM 拓扑发现。不同连接使用不同锁，互不阻塞。
    build_locks: BuildLocks,
    generations: Arc<DashMap<ConnectionId, u64>>,
    generation_clock: Arc<AtomicU64>,
}

#[derive(Clone)]
struct CachedClient {
    generation: u64,
    client: Client,
}

impl PoolCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clone_handle(&self) -> Self {
        self.clone()
    }

    pub async fn get_or_create(&self, config: &ConnectionConfig) -> Result<Client> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        if config.driver != DriverKind::Mongodb {
            return Err(DomainError::InvalidConfig(format!(
                "MongoDriver 不支持 {:?} 类型连接",
                config.driver
            )));
        }

        let generation = self.generation_for_request(&config.id);
        if let Some(client) = self.get_cached(&config.id, generation) {
            debug!(connection_id = %config.id, "client cache hit");
            return Ok(client);
        }

        // 串行化建连 + 双检：避免并发重复建连（各触发一轮 SDAM 发现 → 首开卡顿）
        let build_lock = self.build_lock(&config.id, generation);
        let _guard = build_lock.lock().await;
        if !self.is_current_generation(&config.id, generation) {
            return build_client(config).await;
        }
        if let Some(client) = self.get_cached(&config.id, generation) {
            debug!(connection_id = %config.id, after_lock = true, "client cache hit");
            return Ok(client);
        }

        info!(connection_id = %config.id, name = %config.name, host = %config.host, "creating client");
        let client = build_client(config).await?;
        self.cache_if_current(config.id.clone(), generation, client.clone());
        Ok(client)
    }

    fn cache_if_current(&self, conn_id: ConnectionId, generation: u64, client: Client) {
        if !self.is_current_generation(&conn_id, generation) {
            return;
        }
        match self.clients.entry(conn_id.clone()) {
            Entry::Occupied(mut entry) => {
                if entry.get().generation > generation {
                    return;
                }
                entry.insert(CachedClient { generation, client });
            }
            Entry::Vacant(entry) => {
                entry.insert(CachedClient { generation, client });
            }
        }
        if !self.is_current_generation(&conn_id, generation)
            && let Entry::Occupied(entry) = self.clients.entry(conn_id)
            && entry.get().generation == generation
        {
            entry.remove();
        }
    }

    fn get_cached(&self, conn_id: &ConnectionId, generation: u64) -> Option<Client> {
        self.clients
            .get(conn_id)
            .and_then(|entry| (entry.generation == generation).then(|| entry.client.clone()))
    }

    fn generation_for_request(&self, conn_id: &ConnectionId) -> u64 {
        match self.generations.entry(conn_id.clone()) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let generation = self.next_generation();
                entry.insert(generation);
                generation
            }
        }
    }

    fn is_current_generation(&self, conn_id: &ConnectionId, generation: u64) -> bool {
        self.generations
            .get(conn_id)
            .is_some_and(|entry| *entry == generation)
    }

    fn next_generation(&self) -> u64 {
        loop {
            let generation = self
                .generation_clock
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if generation != 0 {
                return generation;
            }
        }
    }

    fn build_lock(&self, conn_id: &ConnectionId, generation: u64) -> BuildLock {
        self.build_locks
            .entry((conn_id.clone(), generation))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 移除该连接的缓存（编辑配置后调）
    pub fn evict(&self, conn_id: &ConnectionId) {
        let invalidated = self
            .generations
            .remove(conn_id)
            .map(|(_, generation)| generation);
        let Some(invalidated) = invalidated else {
            return;
        };
        self.build_locks
            .retain(|key, _| &key.0 != conn_id || key.1 != invalidated);
        let removed = if let Entry::Occupied(entry) = self.clients.entry(conn_id.clone())
            && entry.get().generation == invalidated
        {
            entry.remove();
            true
        } else {
            false
        };
        if removed {
            info!(connection_id = %conn_id, "client evicted");
        }
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

async fn build_client(config: &ConnectionConfig) -> Result<Client> {
    if config.tls
        && let Some(ca) = config
            .ca_cert_path
            .as_deref()
            .filter(|path| !path.is_empty())
    {
        ramag_infra_tunnel::validate_ca_certificate_file(ca)?;
    }
    // SSH 隧道：启用时改连 127.0.0.1:本地转发端口（就绪探测阻塞，经 spawn_blocking 隔离）
    let cfg_for_tunnel = config.clone();
    let tunnel = tokio::task::spawn_blocking(move || ramag_infra_tunnel::ensure(&cfg_for_tunnel))
        .await
        .map_err(|e| {
            ramag_domain::error::DomainError::QueryFailed(format!("SSH 隧道任务失败：{e}"))
        })??;
    let (host, port) = match tunnel {
        Some((h, p)) => (h, p),
        None => (config.host.clone(), config.port),
    };

    // 用 builder 拼接 Options，避免手写 URI 时的 URL 编码陷阱
    let credential = if config.username.is_empty() {
        None
    } else {
        Some(
            Credential::builder()
                .username(Some(config.username.clone()))
                .password(Some(config.password.clone()))
                // authSource = 用户凭证所在库，独立于「浏览库」database；留空默认 admin。
                // 不再拿 database 顶替——否则指定浏览库就会把认证库指错而登不上
                .source(Some(
                    config
                        .auth_source
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "admin".to_string()),
                ))
                .build(),
        )
    };

    let tls = mongo_tls_options(config);

    let opts = ClientOptions::builder()
        .hosts(vec![ServerAddress::Tcp {
            host,
            port: Some(port),
        }])
        .credential(credential)
        .tls(tls)
        .app_name(Some("ramag".to_string()))
        .connect_timeout(Some(Duration::from_secs(10)))
        .server_selection_timeout(Some(Duration::from_secs(10)))
        // 桌面端并发有限；与 SQL 池一致限制连接数，并回收长期空闲 socket。
        .max_pool_size(Some(8))
        .min_pool_size(Some(0))
        .max_idle_time(Some(Duration::from_secs(60 * 5)))
        .build();

    let client = Client::with_options(opts).map_err(|e| {
        warn!(error = %e, host = %config.host, "build client failed");
        map_mongo_error(e)
    })?;
    Ok(client)
}

/// Rustls 后端不支持只关闭主机名校验，因此 Ca 当前等同 Full；SSH 隧道会关闭验证。
fn mongo_tls_options(config: &ConnectionConfig) -> Option<mongodb::options::Tls> {
    if !config.tls {
        return None;
    }
    let mut options = mongodb::options::TlsOptions::builder().build();
    if let Some(ca) = config
        .ca_cert_path
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        options.ca_file_path = Some(std::path::PathBuf::from(ca));
    }
    let disable_verification = matches!(config.tls_verify, ramag_domain::entities::TlsVerify::None)
        || config.ssh_target.is_some();
    if disable_verification {
        if config.ssh_target.is_some() {
            warn!(host = %config.host, "tls verification disabled over ssh tunnel");
        }
        options.allow_invalid_certificates = Some(true);
    }
    Some(mongodb::options::Tls::Enabled(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_cache_init_empty() {
        let cache = PoolCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn evict_nonexistent_safe() {
        let cache = PoolCache::new();
        let id = ConnectionId::new();
        // 应不报错
        cache.evict(&id);
    }

    #[test]
    fn build_locks_are_per_connection_and_evict_releases_generation_entry() {
        let cache = PoolCache::new();
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();
        let first_generation = cache.generation_for_request(&first_id);
        let second_generation = cache.generation_for_request(&second_id);

        assert!(Arc::ptr_eq(
            &cache.build_lock(&first_id, first_generation),
            &cache.build_lock(&first_id, first_generation)
        ));
        assert!(!Arc::ptr_eq(
            &cache.build_lock(&first_id, first_generation),
            &cache.build_lock(&second_id, second_generation)
        ));
        assert!(!Arc::ptr_eq(
            &cache.build_lock(&first_id, first_generation),
            &cache.build_lock(&first_id, first_generation + 1)
        ));

        assert!(cache.is_current_generation(&first_id, first_generation));
        cache.evict(&first_id);
        assert!(!cache.generations.contains_key(&first_id));
        assert!(!cache.is_current_generation(&first_id, first_generation));

        let recreated_generation = cache.generation_for_request(&first_id);
        assert_ne!(recreated_generation, first_generation);
        assert!(cache.is_current_generation(&first_id, recreated_generation));
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_before_client_build() {
        let cache = PoolCache::new();
        let mut config = ConnectionConfig::new_mongodb("local", "127.0.0.1", 27017);
        config.port = 0;

        assert!(matches!(
            cache.get_or_create(&config).await,
            Err(DomainError::InvalidConfig(_))
        ));
    }

    #[test]
    fn tls_none_disables_certificate_validation_but_ca_does_not() {
        use ramag_domain::entities::TlsVerify;

        let mut config = ConnectionConfig::new_mongodb("local", "127.0.0.1", 27017);
        config.tls = true;
        config.tls_verify = TlsVerify::Ca;
        let Some(mongodb::options::Tls::Enabled(options)) = mongo_tls_options(&config) else {
            panic!("TLS options should be enabled");
        };
        assert_ne!(options.allow_invalid_certificates, Some(true));

        config.tls_verify = TlsVerify::None;
        let Some(mongodb::options::Tls::Enabled(options)) = mongo_tls_options(&config) else {
            panic!("TLS options should be enabled");
        };
        assert_eq!(options.allow_invalid_certificates, Some(true));
    }
}
