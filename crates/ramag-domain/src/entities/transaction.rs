//! SQL 查询会话使用的事务标识。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 事务标识只在当前数据库驱动进程内有效，不代表数据库连接本身。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionId(pub Uuid);

impl TransactionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TransactionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
