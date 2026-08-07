pub const INTERACTIVE_RESULT_WARNING_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_INTERACTIVE_RESULT_BYTES: usize = 256 * 1024 * 1024;

pub const MAX_METADATA_ITEMS: usize = 50_000;
pub const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

pub const TRANSFER_BATCH_ITEMS: usize = 5_000;
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
