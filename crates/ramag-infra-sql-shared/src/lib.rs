//! SQL 类 driver 共享层。每个 driver impl [`SqlBackend`] + [`impl_driver_for!`] 宏即可获得 `Driver` 实现

use ramag_domain::entities::{Column, ForeignKey, Index, Schema, Table};
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

pub use ramag_domain::entities::MAX_METADATA_ITEMS;
pub const METADATA_FETCH_LIMIT: i64 = (MAX_METADATA_ITEMS + 1) as i64;
pub const MAX_METADATA_RESULT_BYTES: usize = ramag_domain::entities::MAX_METADATA_BYTES;

/// 元数据树无法实用地展示超大结果；多取一条作溢出哨兵，超限时明确拒绝而非静默截断。
pub fn ensure_metadata_item_limit(item_count: usize, label: &str) -> Result<()> {
    if item_count > MAX_METADATA_ITEMS {
        return Err(DomainError::QueryFailed(format!(
            "{label}数量超过 {MAX_METADATA_ITEMS} 条安全上限，请缩小数据库范围"
        )));
    }
    Ok(())
}

pub trait MetadataRetainedBytes {
    fn retained_bytes(&self) -> usize;
}

pub fn ensure_metadata_result_limit<T: MetadataRetainedBytes>(
    items: &[T],
    label: &str,
) -> Result<()> {
    ensure_metadata_result_limit_with(items, label, MAX_METADATA_RESULT_BYTES)
}

fn ensure_metadata_result_limit_with<T: MetadataRetainedBytes>(
    items: &[T],
    label: &str,
    max_bytes: usize,
) -> Result<()> {
    ensure_metadata_item_limit(items.len(), label)?;
    let bytes = items.iter().try_fold(0usize, |total, item| {
        total.checked_add(item.retained_bytes())
    });
    if bytes.is_none_or(|bytes| bytes > max_bytes) {
        return Err(DomainError::QueryFailed(format!(
            "{label}元数据超过 {} MiB 内存上限，请缩小数据库范围",
            max_bytes / 1024 / 1024
        )));
    }
    Ok(())
}

fn string_retained_bytes(value: &String) -> usize {
    std::mem::size_of::<String>().saturating_add(value.capacity())
}

fn optional_string_retained_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, string_retained_bytes)
}

impl MetadataRetainedBytes for Schema {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.name.capacity())
            .saturating_add(optional_string_retained_bytes(&self.charset))
            .saturating_add(optional_string_retained_bytes(&self.collation))
    }
}

impl MetadataRetainedBytes for Table {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.name.capacity())
            .saturating_add(self.schema.capacity())
            .saturating_add(optional_string_retained_bytes(&self.comment))
    }
}

impl MetadataRetainedBytes for Column {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.name.capacity())
            .saturating_add(self.data_type.raw_type.capacity())
            .saturating_add(optional_string_retained_bytes(&self.default_value))
            .saturating_add(optional_string_retained_bytes(&self.comment))
    }
}

impl MetadataRetainedBytes for Index {
    fn retained_bytes(&self) -> usize {
        self.columns.iter().fold(
            std::mem::size_of::<Self>()
                .saturating_add(self.name.capacity())
                .saturating_add(
                    self.columns
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                ),
            |total, column| total.saturating_add(column.capacity()),
        )
    }
}

impl MetadataRetainedBytes for ForeignKey {
    fn retained_bytes(&self) -> usize {
        let base = std::mem::size_of::<Self>()
            .saturating_add(self.name.capacity())
            .saturating_add(self.ref_schema.capacity())
            .saturating_add(self.ref_table.capacity())
            .saturating_add(
                self.columns
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            )
            .saturating_add(
                self.ref_columns
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            );
        self.columns
            .iter()
            .chain(&self.ref_columns)
            .fold(base, |total, column| {
                total.saturating_add(column.capacity())
            })
    }
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::Schema;

    use super::{
        MAX_METADATA_ITEMS, MetadataRetainedBytes, ensure_metadata_item_limit,
        ensure_metadata_result_limit_with,
    };

    #[test]
    fn metadata_limit_allows_boundary_and_rejects_overflow() {
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS, "表").is_ok());
        assert!(ensure_metadata_item_limit(MAX_METADATA_ITEMS + 1, "表").is_err());
    }

    #[test]
    fn metadata_memory_limit_has_an_exact_boundary() {
        let items = vec![Schema {
            name: "public".into(),
            charset: None,
            collation: None,
        }];
        let retained = items[0].retained_bytes();

        assert!(ensure_metadata_result_limit_with(&items, "Schema", retained).is_ok());
        assert!(ensure_metadata_result_limit_with(&items, "Schema", retained - 1).is_err());
    }
}
