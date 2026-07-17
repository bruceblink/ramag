//! 异步持久化协调：同 key 只执行最新任务，并串行提交，避免较慢旧写覆盖新状态。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct LatestWriteCoordinator {
    next_revision: Arc<AtomicU64>,
    latest_by_key: Arc<Mutex<HashMap<String, u64>>>,
    write_lock: Arc<futures::lock::Mutex<()>>,
}

impl Default for LatestWriteCoordinator {
    fn default() -> Self {
        Self {
            next_revision: Arc::new(AtomicU64::new(0)),
            latest_by_key: Arc::new(Mutex::new(HashMap::new())),
            write_lock: Arc::new(futures::lock::Mutex::new(())),
        }
    }
}

pub(super) struct LatestWriteTicket {
    key: String,
    revision: u64,
}

impl LatestWriteCoordinator {
    pub(super) fn begin(&self, key: String) -> LatestWriteTicket {
        let revision = self
            .next_revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.with_latest(|latest| {
            latest.insert(key.clone(), revision);
        });
        LatestWriteTicket { key, revision }
    }

    /// 返回 None 表示任务在执行前或执行期间已被同 key 的新任务取代。
    pub(super) async fn run_if_latest<F, Fut, T>(
        &self,
        ticket: &LatestWriteTicket,
        operation: F,
    ) -> Option<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _guard = self.write_lock.lock().await;
        if !self.is_latest(ticket) {
            return None;
        }
        let output = operation().await;
        self.finish_if_latest(ticket).then_some(output)
    }

    fn is_latest(&self, ticket: &LatestWriteTicket) -> bool {
        self.with_latest(|latest| latest.get(&ticket.key).copied() == Some(ticket.revision))
    }

    fn finish_if_latest(&self, ticket: &LatestWriteTicket) -> bool {
        self.with_latest(|latest| {
            if latest.get(&ticket.key).copied() != Some(ticket.revision) {
                return false;
            }
            latest.remove(&ticket.key);
            true
        })
    }

    fn with_latest<T>(&self, operation: impl FnOnce(&mut HashMap<String, u64>) -> T) -> T {
        let mut latest = match self.latest_by_key.lock() {
            Ok(latest) => latest,
            Err(error) => {
                tracing::warn!("latest write coordinator lock poisoned");
                error.into_inner()
            }
        };
        operation(&mut latest)
    }
}

#[cfg(test)]
mod tests {
    use super::LatestWriteCoordinator;

    #[test]
    fn only_latest_waiting_write_runs() {
        futures::executor::block_on(async {
            let coordinator = LatestWriteCoordinator::default();
            let stale = coordinator.begin("repo".into());
            let latest = coordinator.begin("repo".into());

            assert_eq!(
                coordinator.run_if_latest(&stale, || async { 1 }).await,
                None
            );
            assert_eq!(
                coordinator.run_if_latest(&latest, || async { 2 }).await,
                Some(2)
            );
        });
    }

    #[test]
    fn different_keys_are_not_discarded() {
        futures::executor::block_on(async {
            let coordinator = LatestWriteCoordinator::default();
            let first = coordinator.begin("first".into());
            let second = coordinator.begin("second".into());

            assert_eq!(
                coordinator.run_if_latest(&first, || async { 1 }).await,
                Some(1)
            );
            assert_eq!(
                coordinator.run_if_latest(&second, || async { 2 }).await,
                Some(2)
            );
        });
    }
}
