//! 数据库资源边界。

/// 单个交互结果提示线。
pub const INTERACTIVE_RESULT_WARNING_BYTES: usize = 128 * 1024 * 1024;
/// 单个交互结果硬上限。
pub const MAX_INTERACTIVE_RESULT_BYTES: usize = 256 * 1024 * 1024;

/// 元数据查询条目上限。
pub const MAX_METADATA_ITEMS: usize = 50_000;
/// 元数据查询内存上限。
pub const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

/// 传输批次条目上限。
pub const TRANSFER_BATCH_ITEMS: usize = 5_000;
/// 传输批次字节上限。
pub const TRANSFER_BATCH_BYTES: usize = 32 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_database_limits_match_the_approved_boundaries() {
        assert_eq!(INTERACTIVE_RESULT_WARNING_BYTES, 128 * 1024 * 1024);
        assert_eq!(MAX_INTERACTIVE_RESULT_BYTES, 256 * 1024 * 1024);
        assert_eq!(MAX_METADATA_ITEMS, 50_000);
        assert_eq!(MAX_METADATA_BYTES, 64 * 1024 * 1024);
        assert_eq!(TRANSFER_BATCH_ITEMS, 5_000);
        assert_eq!(TRANSFER_BATCH_BYTES, 32 * 1024 * 1024);
    }
}
