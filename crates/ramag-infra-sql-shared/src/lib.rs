//! SQL 类 driver 共享层。每个 driver impl [`SqlBackend`] + [`impl_driver_for!`] 宏即可获得 `Driver` 实现

use ramag_domain::error::{DomainError, Result};

pub mod backend;
pub mod errors;
pub mod macros;
pub mod pool;
pub mod runtime;
pub mod sql;

pub use backend::{
    MAX_QUERY_WARNINGS, SqlBackend, cancel_query_impl, execute_impl, list_columns_impl,
    list_foreign_keys_impl, list_indexes_impl, list_schemas_impl, list_tables_impl,
    server_version_impl, test_connection_impl,
};
pub use pool::PoolCache;
pub use runtime::run_in_tokio;

pub const MAX_METADATA_ITEMS: usize = 50_000;
pub const METADATA_FETCH_LIMIT: i64 = (MAX_METADATA_ITEMS + 1) as i64;

/// 元数据树无法实用地展示超大结果；多取一条作溢出哨兵，超限时明确拒绝而非静默截断。
pub fn ensure_metadata_item_limit(item_count: usize, label: &str) -> Result<()> {
    if item_count > MAX_METADATA_ITEMS {
        return Err(DomainError::QueryFailed(format!(
            "{label}数量超过 {MAX_METADATA_ITEMS} 条安全上限，请缩小数据库范围"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_METADATA_ITEMS, ensure_metadata_item_limit};

    #[test]
    fn metadata_limit_allows_boundary_and_rejects_overflow() {
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS, "表").is_ok());
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS + 1, "表").is_err());
    }
}
