//! 泛型连接池缓存：按 ConnectionId 缓存 `sqlx::Pool<Db>`。DashMap + Arc 多线程安全

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// 当前活跃请求代际。evict 会删除条目，避免永久保留已删除连接的 ID。
    generations: Arc<DashMap<ConnectionId, u64>>,
    /// 全局单调代际号；同一 ID 删除后重建也不会与旧在途请求发生 ABA。
    generation_clock: Arc<AtomicU64>,
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
            generation_clock: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<Db: Database> Clone for PoolCache<Db> {
    fn clone(&self) -> Self {
        Self {
            pools: self.pools.clone(),
            build_locks: self.build_locks.clone(),
            generations: self.generations.clone(),
            generation_clock: self.generation_clock.clone(),
        }
    }
}

impl<Db: Database> PoolCache<Db> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 未命中返回 None（外部 build_pool 后 insert）
    pub fn get(&self, id: &ConnectionId, generation: u64) -> Option<Pool<Db>> {
        self.pools
            .get(id)
            .and_then(|entry| (entry.generation == generation).then(|| entry.pool.clone()))
    }

    pub fn insert(&self, id: ConnectionId, generation: u64, pool: Pool<Db>) {
        if !self.is_current_generation(&id, generation) {
            return;
        }
        match self.pools.entry(id.clone()) {
            Entry::Occupied(mut entry) => {
                // 新代际已先写入时，旧请求不得覆盖它。
                if entry.get().generation > generation {
                    return;
                }
                entry.insert(CachedPool { generation, pool });
            }
            Entry::Vacant(entry) => {
                entry.insert(CachedPool { generation, pool });
            }
        }
        // evict 可能发生在 await 建池期间；只移除本次旧代际，不能误删已写入的新池。
        if !self.is_current_generation(&id, generation)
            && let Entry::Occupied(entry) = self.pools.entry(id)
            && entry.get().generation == generation
        {
            entry.remove();
        }
    }

    pub(crate) fn generation_for_request(&self, id: &ConnectionId) -> u64 {
        match self.generations.entry(id.clone()) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let generation = self.next_generation();
                entry.insert(generation);
                generation
            }
        }
    }

    pub(crate) fn is_current_generation(&self, id: &ConnectionId, generation: u64) -> bool {
        self.generations
            .get(id)
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

    pub(crate) fn build_lock(&self, id: &ConnectionId, generation: u64) -> BuildLock {
        self.build_locks
            .entry((id.clone(), generation))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn evict(&self, id: &ConnectionId) {
        let invalidated = self
            .generations
            .remove(id)
            .map(|(_, generation)| generation);
        let Some(invalidated) = invalidated else {
            return;
        };
        self.build_locks
            .retain(|key, _| &key.0 != id || key.1 != invalidated);
        let removed = if let Entry::Occupied(entry) = self.pools.entry(id.clone())
            && entry.get().generation == invalidated
        {
            entry.remove();
            true
        } else {
            false
        };
        if removed {
            info!(operation = "sql_pool_evict", connection_id = %id, "pool evicted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_locks_are_per_connection_and_evict_releases_generation_entry() {
        let cache = PoolCache::<sqlx::MySql>::new();
        let first_id = ConnectionId::new();
        let second_id = ConnectionId::new();
        let first_generation = cache.generation_for_request(&first_id);
        let second_generation = cache.generation_for_request(&second_id);

        let first = cache.build_lock(&first_id, first_generation);
        let first_again = cache.build_lock(&first_id, first_generation);
        let second = cache.build_lock(&second_id, second_generation);
        let next_generation = cache.build_lock(&first_id, first_generation + 1);
        assert!(Arc::ptr_eq(&first, &first_again));
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &next_generation));

        assert!(cache.is_current_generation(&first_id, first_generation));
        cache.evict(&first_id);
        assert!(!cache.generations.contains_key(&first_id));
        assert!(!cache.is_current_generation(&first_id, first_generation));

        let recreated_generation = cache.generation_for_request(&first_id);
        assert_ne!(recreated_generation, first_generation);
        assert!(cache.is_current_generation(&first_id, recreated_generation));
    }
}
