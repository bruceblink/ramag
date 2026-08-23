//! SQL 驱动的长生命周期事务句柄。

use std::sync::Arc;

use dashmap::DashMap;
use ramag_domain::entities::{ConnectionId, TransactionId};
use ramag_domain::error::{DomainError, Result};
use sqlx::{Database, Transaction};
use tokio::sync::Mutex;

/// 单个连接最多保留的活动事务数，避免异常 UI 会话无限占用连接池连接。
pub const MAX_ACTIVE_TRANSACTIONS_PER_CONNECTION: usize = 4;

type TransactionSlot<Db> = Arc<Mutex<Option<Transaction<'static, Db>>>>;
type TransactionKey = (ConnectionId, TransactionId);

/// 事务句柄按连接归属隔离；事务 ID 离开当前驱动进程后不可复用。
pub struct TransactionStore<Db: Database> {
    active: Arc<DashMap<TransactionKey, TransactionSlot<Db>>>,
}

impl<Db: Database> Clone for TransactionStore<Db> {
    fn clone(&self) -> Self {
        Self {
            active: self.active.clone(),
        }
    }
}

impl<Db: Database> Default for TransactionStore<Db> {
    fn default() -> Self {
        Self {
            active: Arc::new(DashMap::new()),
        }
    }
}

impl<Db: Database> TransactionStore<Db> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a newly begun transaction and returns a bounded opaque ID.
    pub fn insert(
        &self,
        connection_id: ConnectionId,
        transaction: Transaction<'static, Db>,
    ) -> Result<TransactionId> {
        let active_for_connection = self
            .active
            .iter()
            .filter(|entry| entry.key().0 == connection_id)
            .count();
        if active_for_connection >= MAX_ACTIVE_TRANSACTIONS_PER_CONNECTION {
            return Err(DomainError::QueryFailed(format!(
                "单个连接最多同时保留 {MAX_ACTIVE_TRANSACTIONS_PER_CONNECTION} 个事务"
            )));
        }

        let transaction_id = TransactionId::new();
        self.active.insert(
            (connection_id, transaction_id.clone()),
            Arc::new(Mutex::new(Some(transaction))),
        );
        Ok(transaction_id)
    }

    pub fn get(
        &self,
        connection_id: &ConnectionId,
        transaction_id: &TransactionId,
    ) -> Option<TransactionSlot<Db>> {
        self.active
            .get(&(connection_id.clone(), transaction_id.clone()))
            .map(|entry| entry.value().clone())
    }

    pub fn remove(
        &self,
        connection_id: &ConnectionId,
        transaction_id: &TransactionId,
    ) -> Option<TransactionSlot<Db>> {
        self.active
            .remove(&(connection_id.clone(), transaction_id.clone()))
            .map(|(_, slot)| slot)
    }

    /// Removes all handles for an evicted connection; dropping each open transaction rolls it back.
    pub fn clear_connection(&self, connection_id: &ConnectionId) {
        self.active.retain(|key, _| &key.0 != connection_id);
    }

    #[cfg(test)]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
}
