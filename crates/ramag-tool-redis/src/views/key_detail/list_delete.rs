//! List 元素删除的原子校验与结果分类。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ramag_domain::entities::RedisValue;

/// 在 Redis 服务器内原子校验“序号 + 旧值”后删除，避免 LREM 对重复值删错位置。
/// 返回 1=已删除，0=序号不存在，-1=该位置内容已变化。
pub(super) const DELETE_LIST_INDEX_SCRIPT: &str = r#"
local current = redis.call('LINDEX', KEYS[1], ARGV[1])
if not current then return 0 end
if current ~= ARGV[2] then return -1 end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
return redis.call('LREM', KEYS[1], 1, ARGV[3])
"#;

pub(super) fn list_delete_marker() -> String {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    format!("\0ramag-list-delete:{}:{nanos}:{nonce}", std::process::id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListDeleteStatus {
    Deleted,
    Missing,
    Changed,
    Unexpected,
}

pub(super) fn list_delete_status(response: &RedisValue) -> ListDeleteStatus {
    match response {
        RedisValue::Int(1) => ListDeleteStatus::Deleted,
        RedisValue::Int(0) => ListDeleteStatus::Missing,
        RedisValue::Int(-1) => ListDeleteStatus::Changed,
        _ => ListDeleteStatus::Unexpected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_delete_response_is_classified_explicitly() {
        assert_eq!(
            list_delete_status(&RedisValue::Int(1)),
            ListDeleteStatus::Deleted
        );
        assert_eq!(
            list_delete_status(&RedisValue::Int(0)),
            ListDeleteStatus::Missing
        );
        assert_eq!(
            list_delete_status(&RedisValue::Int(-1)),
            ListDeleteStatus::Changed
        );
        assert_eq!(
            list_delete_status(&RedisValue::Text("1".into())),
            ListDeleteStatus::Unexpected
        );
    }

    #[test]
    fn list_delete_markers_are_unique_and_binary_prefixed() {
        let first = list_delete_marker();
        let second = list_delete_marker();
        assert_ne!(first, second);
        assert!(first.starts_with('\0'));
    }
}
