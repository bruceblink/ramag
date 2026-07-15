//! 图片解密缓存：图片落盘是密文，UI 不能直接 img(path)。
//! 渲染时同步查缓存；miss 则异步解密解码填充 + notify，下一帧显示。
//! 内部 RefCell，故 render（&self）可填发起加载

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use gpui::Image;

/// 抽屉最多同时展示 60 张卡片；留出余量给主视图详情，同时限制长时间运行的内存增长。
const MAX_ENTRIES: usize = 96;
/// 同时限制编码图片字节总量，避免少量大图绕过纯条数上限。
const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
/// 失败记录也必须有界；旧失败被淘汰后允许未来重试（文件可能已被恢复或修复）。
const MAX_FAILED_ENTRIES: usize = 256;

struct CacheEntry {
    image: Arc<Image>,
    encoded_bytes: usize,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, CacheEntry>,
    /// 最近使用的 key 在队尾。
    order: VecDeque<String>,
    encoded_bytes: usize,
}

#[derive(Default)]
pub(crate) struct ImageCache {
    cache: RefCell<CacheState>,
    loading: RefCell<HashSet<String>>,
    /// 解密 / 解码失败的路径：显示失败占位，不再每帧无限重试
    failed: RefCell<HashSet<String>>,
    failed_order: RefCell<VecDeque<String>>,
}

impl ImageCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 同步取已解密图片
    pub(crate) fn peek(&self, path: &str) -> Option<Arc<Image>> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.entries.get(path).map(|entry| entry.image.clone())?;
        touch(&mut cache.order, path);
        Some(image)
    }

    /// 该路径是否已判定失败（渲染显示「无法解密」占位）
    pub(crate) fn is_failed(&self, path: &str) -> bool {
        self.failed.borrow().contains(path)
    }

    /// 抢加载权：未缓存、未在加载、未失败才返回 true（防同路径重复 spawn / 失败风暴）
    pub(crate) fn begin_load(&self, path: &str) -> bool {
        if self.cache.borrow().entries.contains_key(path) || self.failed.borrow().contains(path) {
            return false;
        }
        self.loading.borrow_mut().insert(path.to_string())
    }

    pub(crate) fn insert(&self, path: String, image: Arc<Image>, encoded_bytes: usize) {
        self.loading.borrow_mut().remove(&path);
        if self.failed.borrow_mut().remove(&path) {
            remove_key(&mut self.failed_order.borrow_mut(), &path);
        }

        let mut cache = self.cache.borrow_mut();
        if let Some(previous) = cache.entries.remove(&path) {
            cache.encoded_bytes = cache.encoded_bytes.saturating_sub(previous.encoded_bytes);
            remove_key(&mut cache.order, &path);
        }
        cache.encoded_bytes = cache.encoded_bytes.saturating_add(encoded_bytes);
        cache.entries.insert(
            path.clone(),
            CacheEntry {
                image,
                encoded_bytes,
            },
        );
        cache.order.push_back(path);
        evict_to_limits(&mut cache);
    }

    /// 加载失败：记入失败集（本次会话内不再重试，显示失败占位）
    pub(crate) fn fail(&self, path: &str) {
        self.loading.borrow_mut().remove(path);
        let mut failed = self.failed.borrow_mut();
        if failed.insert(path.to_string()) {
            let mut order = self.failed_order.borrow_mut();
            order.push_back(path.to_string());
            while failed.len() > MAX_FAILED_ENTRIES {
                let Some(oldest) = order.pop_front() else {
                    break;
                };
                failed.remove(&oldest);
            }
        }
    }
}

fn touch(order: &mut VecDeque<String>, key: &str) {
    let owned = order
        .iter()
        .position(|entry| entry == key)
        .and_then(|index| order.remove(index))
        .unwrap_or_else(|| key.to_string());
    order.push_back(owned);
}

fn remove_key(order: &mut VecDeque<String>, key: &str) {
    if let Some(index) = order.iter().position(|entry| entry == key) {
        order.remove(index);
    }
}

fn evict_to_limits(cache: &mut CacheState) {
    while cache.entries.len() > MAX_ENTRIES || cache.encoded_bytes > MAX_ENCODED_BYTES {
        let Some(oldest) = cache.order.pop_front() else {
            break;
        };
        if let Some(entry) = cache.entries.remove(&oldest) {
            cache.encoded_bytes = cache.encoded_bytes.saturating_sub(entry.encoded_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ImageFormat;

    fn image() -> Arc<Image> {
        Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new()))
    }

    #[test]
    fn cache_evicts_least_recently_used_entry() {
        let cache = ImageCache::new();
        for index in 0..MAX_ENTRIES {
            cache.insert(format!("{index}.png"), image(), 1);
        }
        assert!(cache.peek("0.png").is_some());

        cache.insert("extra.png".into(), image(), 1);

        assert!(cache.peek("0.png").is_some());
        assert!(cache.peek("1.png").is_none());
        assert!(cache.peek("extra.png").is_some());
    }

    #[test]
    fn cache_respects_encoded_byte_limit() {
        let cache = ImageCache::new();
        cache.insert("first.png".into(), image(), MAX_ENCODED_BYTES);
        cache.insert("second.png".into(), image(), 1);

        assert!(cache.peek("first.png").is_none());
        assert!(cache.peek("second.png").is_some());
    }

    #[test]
    fn failed_paths_are_bounded_and_old_entries_can_retry() {
        let cache = ImageCache::new();
        for index in 0..=MAX_FAILED_ENTRIES {
            cache.fail(&format!("{index}.png"));
        }

        assert!(cache.begin_load("0.png"));
        assert!(!cache.begin_load(&format!("{MAX_FAILED_ENTRIES}.png")));
    }
}
