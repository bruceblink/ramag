//! 数据库客户端共用的资源边界。

/// 单个交互结果接近该常驻内存时提示用户收窄查询。
pub const INTERACTIVE_RESULT_WARNING_BYTES: usize = 128 * 1024 * 1024;
/// 单个交互结果允许保留的最大常驻内存。
pub const MAX_INTERACTIVE_RESULT_BYTES: usize = 256 * 1024 * 1024;

/// 一次元数据查询允许返回的最大条目数。
pub const MAX_METADATA_ITEMS: usize = 50_000;
/// 一次元数据查询允许保留的最大常驻内存。
pub const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

/// 导入导出一次批处理允许包含的最大条目数。
pub const TRANSFER_BATCH_ITEMS: usize = 5_000;
/// 导入导出一次批处理允许包含的最大内容字节数。
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
