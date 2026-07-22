//! Redis 连接缓存：键为 `(ConnectionId, db)`（SELECT 是连接级状态，不能跨 db 共享）。
//! ConnectionManager 自动重连 + 多路复用，clone 是 Arc 廉价复制。当前仅 standalone

use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use ramag_domain::entities::{ConnectionConfig, ConnectionId, DriverKind};
use ramag_domain::error::{DomainError, Result};
use redis::aio::ConnectionManager;
use redis::{Client, ConnectionAddr, ConnectionInfo, ProtocolVersion, RedisConnectionInfo};
use tracing::{debug, info, warn};

use crate::errors::map_redis_error;

type BuildLock = Arc<tokio::sync::Mutex<()>>;
type BuildLocks = Arc<DashMap<(PoolKey, u64), BuildLock>>;

const MAX_CACHED_DBS_PER_CONNECTION: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub conn_id: ConnectionId,
    pub db: u8,
}

impl PoolKey {
    pub fn new(conn_id: ConnectionId, db: u8) -> Self {
        Self { conn_id, db }
    }
}

#[derive(Clone, Default)]
pub struct PoolCache {
    pools: Arc<DashMap<PoolKey, CachedManager>>,
    build_locks: BuildLocks,
    generations: Arc<DashMap<ConnectionId, u64>>,
    generation_clock: Arc<AtomicU64>,
    access_clock: Arc<AtomicU64>,
}

#[derive(Clone)]
struct CachedManager {
    generation: u64,
    last_used: u64,
    manager: ConnectionManager,
}

impl PoolCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clone_handle(&self) -> Self {
        self.clone()
    }

    pub async fn get_or_create(
        &self,
        config: &ConnectionConfig,
        db: u8,
    ) -> Result<ConnectionManager> {
        config.validate().map_err(DomainError::InvalidConfig)?;
        if config.driver != DriverKind::Redis {
            return Err(DomainError::InvalidConfig(format!(
                "RedisDriver 不支持 {:?} 类型连接",
                config.driver
            )));
        }

        let key = PoolKey::new(config.id.clone(), db);
        let generation = self.generation_for_request(&config.id);

        if let Some(manager) = self.get_cached(&key, generation) {
            self.enforce_pool_limit(&config.id, &key, generation);
            debug!(connection_id = %config.id, db, "connection pool cache hit");
            return Ok(manager);
        }

        let build_lock = self.build_lock(&key, generation);
        let _guard = build_lock.lock().await;
        if !self.is_current_generation(&config.id, generation) {
            return build_connection_manager(config, db).await;
        }
        if let Some(manager) = self.get_cached(&key, generation) {
            self.enforce_pool_limit(&config.id, &key, generation);
            debug!(connection_id = %config.id, db, after_lock = true, "connection pool cache hit");
            return Ok(manager);
        }

        info!(connection_id = %config.id, name = %config.name, host = %config.host, db, "creating connection manager");
        let mgr = build_connection_manager(config, db).await?;
        if self.cache_if_current(key.clone(), generation, mgr.clone()) {
            self.enforce_pool_limit(&config.id, &key, generation);
        }
        Ok(mgr)
    }

    fn cache_if_current(&self, key: PoolKey, generation: u64, manager: ConnectionManager) -> bool {
        if !self.is_current_generation(&key.conn_id, generation) {
            return false;
        }
        match self.pools.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                if entry.get().generation > generation {
                    return false;
                }
                entry.insert(CachedManager {
                    generation,
                    last_used: self.next_access(),
                    manager,
                });
            }
            Entry::Vacant(entry) => {
                entry.insert(CachedManager {
                    generation,
                    last_used: self.next_access(),
                    manager,
                });
            }
        }
        if !self.is_current_generation(&key.conn_id, generation) {
            if let Entry::Occupied(entry) = self.pools.entry(key)
                && entry.get().generation == generation
            {
                entry.remove();
            }
            return false;
        }
        true
    }

    fn get_cached(&self, key: &PoolKey, generation: u64) -> Option<ConnectionManager> {
        match self.pools.entry(key.clone()) {
            Entry::Occupied(mut entry) if entry.get().generation == generation => {
                entry.get_mut().last_used = self.next_access();
                Some(entry.get().manager.clone())
            }
            Entry::Occupied(_) => None,
            Entry::Vacant(_) => None,
        }
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

    fn next_access(&self) -> u64 {
        self.access_clock.fetch_add(1, Ordering::Relaxed)
    }

    fn enforce_pool_limit(&self, conn_id: &ConnectionId, keep: &PoolKey, generation: u64) {
        let entries = self
            .pools
            .iter()
            .filter(|entry| {
                &entry.key().conn_id == conn_id && entry.value().generation == generation
            })
            .map(|entry| (entry.key().clone(), entry.value().last_used))
            .collect();
        let candidates = lru_eviction_candidates(entries, keep, MAX_CACHED_DBS_PER_CONNECTION);
        let mut evicted = 0usize;
        for (key, observed_last_used) in candidates {
            if let Entry::Occupied(entry) = self.pools.entry(key)
                && entry.get().generation == generation
                && entry.get().last_used == observed_last_used
            {
                entry.remove();
                evicted += 1;
            }
        }
        if evicted > 0 {
            debug!(connection_id = %conn_id, evicted, "database pools evicted by LRU");
        }
    }

    fn build_lock(&self, key: &PoolKey, generation: u64) -> BuildLock {
        // 仅 map 自身持有的锁已无等待者或执行者，可安全回收；避免轮询大量 DB 后锁表增长。
        self.build_locks
            .retain(|_, lock| Arc::strong_count(lock) > 1);
        self.build_locks
            .entry((key.clone(), generation))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 移除该连接所有 db 的缓存（编辑配置后调）
    pub fn evict_all_dbs(&self, conn_id: &ConnectionId) {
        let invalidated = self
            .generations
            .remove(conn_id)
            .map(|(_, generation)| generation);
        let Some(invalidated) = invalidated else {
            return;
        };
        self.build_locks
            .retain(|key, _| &key.0.conn_id != conn_id || key.1 != invalidated);
        let mut evicted = 0usize;
        self.pools.retain(|key, cached| {
            let remove = &key.conn_id == conn_id && cached.generation == invalidated;
            evicted += usize::from(remove);
            !remove
        });
        if evicted > 0 {
            info!(connection_id = %conn_id, evicted, "connection pools evicted");
        }
    }
}

fn lru_eviction_candidates(
    mut entries: Vec<(PoolKey, u64)>,
    keep: &PoolKey,
    max_entries: usize,
) -> Vec<(PoolKey, u64)> {
    let remove_count = entries.len().saturating_sub(max_entries);
    if remove_count == 0 {
        return Vec::new();
    }
    entries.retain(|(key, _)| key != keep);
    entries.sort_by_key(|(key, last_used)| (*last_used, key.db));
    entries.truncate(remove_count);
    entries
}

async fn build_connection_manager(config: &ConnectionConfig, db: u8) -> Result<ConnectionManager> {
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
        .map_err(|e| DomainError::QueryFailed(format!("SSH 隧道任务失败：{e}")))??;
    let (host, port) = match tunnel {
        Some((h, p)) => (h, p),
        None => (config.host.clone(), config.port),
    };
    let info = build_connection_info(config, db, host, port);

    // 自定义 CA：读 PEM 字节经 build_with_tls 注入信任根（自签场景）；否则走系统信任链
    let client = if config.tls
        && let Some(ca) = config.ca_cert_path.as_deref().filter(|s| !s.is_empty())
    {
        let pem = read_ca_certificate(ca)?;
        Client::build_with_tls(
            info,
            redis::TlsCertificates {
                client_tls: None,
                root_cert: Some(pem),
            },
        )
        .map_err(|e| {
            warn!(error = %e, host = %config.host, "build TLS client failed");
            map_redis_error(e)
        })?
    } else {
        Client::open(info).map_err(|e| {
            warn!(error = %e, host = %config.host, "build client failed");
            map_redis_error(e)
        })?
    };

    // 设连接 / 应答超时避免 GUI 卡死
    let mgr = ConnectionManager::new_with_config(
        client,
        redis::aio::ConnectionManagerConfig::new()
            .set_connection_timeout(Duration::from_secs(10))
            .set_response_timeout(Duration::from_secs(30)),
    )
    .await
    .map_err(|e| {
        warn!(error = %e, host = %config.host, "open connection manager failed");
        map_redis_error(e)
    })?;

    Ok(mgr)
}

fn read_ca_certificate(path: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|error| {
        DomainError::InvalidConfig(format!("读取 CA 证书失败（{path}）：{error}"))
    })?;
    let mut pem = Vec::with_capacity(16 * 1024);
    file.take((ramag_infra_tunnel::MAX_CA_CERT_BYTES + 1) as u64)
        .read_to_end(&mut pem)
        .map_err(|error| {
            DomainError::InvalidConfig(format!("读取 CA 证书失败（{path}）：{error}"))
        })?;
    if pem.len() > ramag_infra_tunnel::MAX_CA_CERT_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "CA 证书超过 {} MiB 安全上限（{path}）",
            ramag_infra_tunnel::MAX_CA_CERT_BYTES / 1024 / 1024
        )));
    }
    if pem.is_empty() {
        return Err(DomainError::InvalidConfig(format!(
            "CA 证书文件为空（{path}）"
        )));
    }
    Ok(pem)
}

/// TCP 或 TLS（config.tls）；Unix Socket 暂不支持。
/// host / port 由调用方传入（可能已被 SSH 隧道替换为本地转发地址）
fn build_connection_info(
    config: &ConnectionConfig,
    db: u8,
    host: String,
    port: u16,
) -> ConnectionInfo {
    let username = if config.username.is_empty() {
        None
    } else {
        Some(config.username.clone())
    };
    let password = if config.password.is_empty() {
        None
    } else {
        Some(config.password.clone())
    };

    // TLS：rustls 校验时总是连带主机名（无「只验链不验名」档），因此
    // Ca 与 Full 同为严格校验；None 档或经 SSH 隧道（实际连 127.0.0.1，校验必败、
    // 隧道本身已加密）时置 insecure 仅加密。tls_params 留 None——自定义 CA 在
    // build_with_tls 注入
    let addr = if config.tls {
        let insecure = matches!(config.tls_verify, ramag_domain::entities::TlsVerify::None)
            || config.ssh_target.is_some();
        if insecure && config.ssh_target.is_some() {
            warn!(host = %config.host, "TLS verification disabled over SSH tunnel");
        }
        ConnectionAddr::TcpTls {
            host,
            port,
            insecure,
            tls_params: None,
        }
    } else {
        ConnectionAddr::Tcp(host, port)
    };

    ConnectionInfo {
        addr,
        redis: RedisConnectionInfo {
            db: db as i64,
            username,
            password,
            protocol: ProtocolVersion::RESP2,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn pool_key_eq_and_hash() {
        let id = ConnectionId::new();
        let a = PoolKey::new(id.clone(), 0);
        let b = PoolKey::new(id.clone(), 0);
        let c = PoolKey::new(id, 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn build_locks_are_per_database_and_evict_releases_generation_entry() {
        let cache = PoolCache::new();
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();
        let first_generation = cache.generation_for_request(&first_id);
        let second_generation = cache.generation_for_request(&second_id);
        let first = PoolKey::new(first_id.clone(), 0);
        let first_again = PoolKey::new(first_id.clone(), 0);
        let other_db = PoolKey::new(first_id.clone(), 1);
        let other_connection = PoolKey::new(second_id, 0);

        assert!(Arc::ptr_eq(
            &cache.build_lock(&first, first_generation),
            &cache.build_lock(&first_again, first_generation)
        ));
        assert!(!Arc::ptr_eq(
            &cache.build_lock(&first, first_generation),
            &cache.build_lock(&other_db, first_generation)
        ));
        assert!(!Arc::ptr_eq(
            &cache.build_lock(&first, first_generation),
            &cache.build_lock(&other_connection, second_generation)
        ));
        assert!(!Arc::ptr_eq(
            &cache.build_lock(&first, first_generation),
            &cache.build_lock(&first, first_generation + 1)
        ));

        assert!(cache.is_current_generation(&first_id, first_generation));
        cache.evict_all_dbs(&first_id);
        assert!(!cache.generations.contains_key(&first_id));
        assert!(!cache.is_current_generation(&first_id, first_generation));

        let recreated_generation = cache.generation_for_request(&first_id);
        assert_ne!(recreated_generation, first_generation);
        assert!(cache.is_current_generation(&first_id, recreated_generation));
    }

    #[test]
    fn idle_build_locks_are_pruned_on_next_cache_miss() {
        let cache = PoolCache::new();
        let first = PoolKey::new(ConnectionId::new(), 0);
        let second = PoolKey::new(ConnectionId::new(), 0);

        let first_generation = cache.generation_for_request(&first.conn_id);
        let second_generation = cache.generation_for_request(&second.conn_id);
        let idle = cache.build_lock(&first, first_generation);
        drop(idle);
        assert!(
            cache
                .build_locks
                .contains_key(&(first.clone(), first_generation))
        );

        let _active = cache.build_lock(&second, second_generation);
        assert!(!cache.build_locks.contains_key(&(first, first_generation)));
        assert!(cache.build_locks.contains_key(&(second, second_generation)));
    }

    #[test]
    fn build_info_no_auth() {
        let cfg = ConnectionConfig::new_redis("local", "127.0.0.1", 6379);
        let info = build_connection_info(&cfg, 0, cfg.host.clone(), cfg.port);
        assert!(matches!(info.addr, ConnectionAddr::Tcp(_, 6379)));
        assert_eq!(info.redis.db, 0);
        assert!(info.redis.username.is_none());
        assert!(info.redis.password.is_none());
    }

    #[test]
    fn build_info_with_acl() {
        let mut cfg = ConnectionConfig::new_redis("local", "127.0.0.1", 6379);
        cfg.username = "default".into();
        cfg.password = "secret".into();
        let info = build_connection_info(&cfg, 3, cfg.host.clone(), cfg.port);
        assert_eq!(info.redis.db, 3);
        assert_eq!(info.redis.username.as_deref(), Some("default"));
        assert_eq!(info.redis.password.as_deref(), Some("secret"));
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_before_connection_build() {
        let cache = PoolCache::new();
        let mut config = ConnectionConfig::new_redis("local", "127.0.0.1", 6379);
        config.port = 0;

        assert!(matches!(
            cache.get_or_create(&config, 0).await,
            Err(DomainError::InvalidConfig(_))
        ));
    }

    #[test]
    fn ca_certificate_read_is_bounded_and_rejects_empty_files()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ramag-redis-ca-test-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path_text = path.to_string_lossy();

        std::fs::write(&path, b"certificate")?;
        assert_eq!(read_ca_certificate(&path_text)?, b"certificate");

        std::fs::write(&path, [])?;
        assert!(read_ca_certificate(&path_text).is_err());

        std::fs::write(&path, vec![0; ramag_infra_tunnel::MAX_CA_CERT_BYTES + 1])?;
        assert!(read_ca_certificate(&path_text).is_err());

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn lru_eviction_keeps_current_database_and_removes_oldest_others() {
        let id = ConnectionId::new();
        let keep = PoolKey::new(id.clone(), 0);
        let entries = (0u8..10)
            .map(|db| (PoolKey::new(id.clone(), db), u64::from(db)))
            .collect();

        let candidates = lru_eviction_candidates(entries, &keep, 8);

        assert_eq!(
            candidates
                .into_iter()
                .map(|(key, _)| key.db)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
