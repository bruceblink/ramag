//! 泛型连接池缓存：按 ConnectionId 缓存 `sqlx::Pool<Db>`。DashMap + Arc 多线程安全

use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use ramag_domain::entities::ConnectionId;
use sqlx::{Database, Pool};
use tracing::info;

type BuildLock = Arc<tokio::sync::Mutex<()>>;
type BuildLocks = Arc<DashMap<(ConnectionId, u64), BuildLock>>;

pub struct PoolCache<Db: Database> {
    pools: Arc<DashMap<ConnectionId, CachedPool<Db>>>,
    /// 同一连接首次建池串行化；不同连接使用不同锁，可并行握手。
    build_locks: BuildLocks,
    /// evict 后递增。旧配置的在途建池即使晚回包，也只会写入旧代际，后续不会命中。
    generations: Arc<DashMap<ConnectionId, u64>>,
}

struct CachedPool<Db: Database> {
    generation: u64,
    pool: Pool<Db>,
}

impl<Db: Database> Default for PoolCache<Db> {
    fn default() -> Self {
        Self {
            pools: Arc::new(DashMap::new()),
            build_locks: Arc::new(DashMap::new()),
            generations: Arc::new(DashMap::new()),
        }
    }
}

impl<Db: Database> Clone for PoolCache<Db> {
    fn clone(&self) -> Self {
        Self {
            pools: self.pools.clone(),
            build_locks: self.build_locks.clone(),
            generations: self.generations.clone(),
        }
    }
}

impl<Db: Database> PoolCache<Db> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 未命中返回 None（外部 build_pool 后 insert）
    pub fn get(&self, id: &ConnectionId, generation: u64) -> Option<Pool<Db>> {
        match self.pools.entry(id.clone()) {
            Entry::Occupied(entry) if entry.get().generation == generation => {
                Some(entry.get().pool.clone())
            }
            Entry::Occupied(entry) => {
                entry.remove();
                None
            }
            Entry::Vacant(_) => None,
        }
    }

    pub fn insert(&self, id: ConnectionId, generation: u64, pool: Pool<Db>) {
        self.pools
            .insert(id.clone(), CachedPool { generation, pool });
        // evict 可能发生在 await 建池期间；只移除本次旧代际，不能误删已写入的新池。
        if self.generation(&id) != generation
            && let Entry::Occupied(entry) = self.pools.entry(id)
            && entry.get().generation == generation
        {
            entry.remove();
        }
    }

    pub(crate) fn generation(&self, id: &ConnectionId) -> u64 {
        self.generations.get(id).map_or(0, |entry| *entry)
    }

    pub(crate) fn build_lock(&self, id: &ConnectionId, generation: u64) -> BuildLock {
        self.build_locks
            .entry((id.clone(), generation))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn evict(&self, id: &ConnectionId) {
        self.generations
            .entry(id.clone())
            .and_modify(|generation| *generation = generation.wrapping_add(1))
            .or_insert(1);
        self.build_locks.retain(|key, _| &key.0 != id);
        if self.pools.remove(id).is_some() {
            info!(connection_id = %id, "pool evicted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_locks_are_per_connection_and_evict_advances_generation() {
        let cache = PoolCache::<sqlx::MySql>::new();
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();

        let first = cache.build_lock(&first_id, 0);
        let first_again = cache.build_lock(&first_id, 0);
        let second = cache.build_lock(&second_id, 0);
        let next_generation = cache.build_lock(&first_id, 1);
        assert!(Arc::ptr_eq(&first, &first_again));
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &next_generation));

        assert_eq!(cache.generation(&first_id), 0);
        cache.evict(&first_id);
        assert_eq!(cache.generation(&first_id), 1);
    }
}
