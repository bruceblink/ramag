//! 来源应用图标的有界 LRU 缓存。正负结果都缓存，避免重复调用平台图标 API；
//! 同时限制条目数和 PNG 字节数，防止长时间运行后常驻内存持续增长。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct IconCache {
    entries: HashMap<String, Option<Arc<Vec<u8>>>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl IconCache {
    /// 外层 Option 区分“未缓存”和“已缓存但无图标”。
    pub(crate) fn get(&mut self, key: &str) -> Option<Option<Arc<Vec<u8>>>> {
        let value = self.entries.get(key)?.clone();
        touch(&mut self.order, key);
        Some(value)
    }

    pub(crate) fn insert(&mut self, key: String, value: Option<Arc<Vec<u8>>>) {
        let value_bytes = encoded_len(&value);
        if value_bytes > MAX_BYTES {
            return;
        }

        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(encoded_len(&previous));
            remove_key(&mut self.order, &key);
        }
        self.bytes = self.bytes.saturating_add(value_bytes);
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
        self.evict_to_limits();
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(value) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(encoded_len(&value));
            }
        }
    }
}

fn encoded_len(value: &Option<Arc<Vec<u8>>>) -> usize {
    value.as_ref().map_or(0, |bytes| bytes.len())
}

fn touch(order: &mut VecDeque<String>, key: &str) {
    if let Some(index) = order.iter().position(|entry| entry == key)
        && let Some(key) = order.remove(index)
    {
        order.push_back(key);
    }
}

fn remove_key(order: &mut VecDeque<String>, key: &str) {
    if let Some(index) = order.iter().position(|entry| entry == key) {
        order.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(size: usize) -> Option<Arc<Vec<u8>>> {
        Some(Arc::new(vec![0; size]))
    }

    #[test]
    fn cache_evicts_least_recently_used_entry() {
        let mut cache = IconCache::default();
        for index in 0..MAX_ENTRIES {
            cache.insert(format!("app-{index}"), icon(1));
        }
        assert!(cache.get("app-0").is_some());

        cache.insert("new-app".into(), icon(1));

        assert!(cache.get("app-0").is_some());
        assert!(cache.get("app-1").is_none());
        assert!(cache.get("new-app").is_some());
    }

    #[test]
    fn cache_respects_byte_limit_and_keeps_negative_results() {
        let mut cache = IconCache::default();
        cache.insert("missing".into(), None);
        assert!(matches!(cache.get("missing"), Some(None)));

        cache.insert("first".into(), icon(MAX_BYTES / 2 + 1));
        cache.insert("second".into(), icon(MAX_BYTES / 2 + 1));

        assert!(cache.bytes <= MAX_BYTES);
        assert!(cache.get("first").is_none());
        assert!(cache.get("second").is_some());
    }

    #[test]
    fn oversized_icon_is_not_cached() {
        let mut cache = IconCache::default();
        cache.insert("huge".into(), icon(MAX_BYTES + 1));
        assert!(cache.get("huge").is_none());
    }
}
