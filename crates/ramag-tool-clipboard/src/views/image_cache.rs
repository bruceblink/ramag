//! 图片解密缓存：图片落盘是密文，UI 不能直接 img(path)。
//! 渲染时同步查缓存；miss 则异步解密解码填充 + notify，下一帧显示。
//! 内部 RefCell，故 render（&self）可填发起加载

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use gpui::Image;

/// 抽屉最多同时展示 60 张卡片；留出余量给主视图详情，同时限制长时间运行的内存增长。
const MAX_ENTRIES: usize = 96;
/// 按 PNG 编码字节 + RGBA 像素估算驻留内存，避免压缩率极高的大图绕过缓存上限。
const MAX_RETAINED_BYTES: usize = 256 * 1024 * 1024;
/// 极端长宽即使总像素不大，也可能触发解码器或纹理后端的异常大尺寸路径。
const MAX_IMAGE_DIMENSION: u32 = 16_384;
/// 失败记录也必须有界；旧失败被淘汰后允许未来重试（文件可能已被恢复或修复）。
const MAX_FAILED_ENTRIES: usize = 256;
/// 快速滚动时限制同时解密 / 解码的图片数，避免任务与临时字节堆积。
const MAX_IN_FLIGHT_LOADS: usize = 4;

struct CacheEntry {
    image: Arc<Image>,
    retained_bytes: usize,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, CacheEntry>,
    /// 最近使用的 key 在队尾。
    order: VecDeque<String>,
    retained_bytes: usize,
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

    /// 该路径是否已判定失败（缺失、损坏或尺寸过大时不再每帧重试）
    pub(crate) fn is_failed(&self, path: &str) -> bool {
        self.failed.borrow().contains(path)
    }

    /// 抢加载权：未缓存、未在加载、未失败才返回 true（防同路径重复 spawn / 失败风暴）
    pub(crate) fn begin_load(&self, path: &str) -> bool {
        if self.cache.borrow().entries.contains_key(path) || self.failed.borrow().contains(path) {
            return false;
        }
        let mut loading = self.loading.borrow_mut();
        if loading.len() >= MAX_IN_FLIGHT_LOADS {
            return false;
        }
        loading.insert(path.to_string())
    }

    pub(crate) fn insert(&self, path: String, image: Arc<Image>, retained_bytes: usize) {
        self.loading.borrow_mut().remove(&path);
        if retained_bytes > MAX_RETAINED_BYTES {
            self.fail(&path);
            return;
        }
        if self.failed.borrow_mut().remove(&path) {
            remove_key(&mut self.failed_order.borrow_mut(), &path);
        }

        let mut cache = self.cache.borrow_mut();
        if let Some(previous) = cache.entries.remove(&path) {
            cache.retained_bytes = cache.retained_bytes.saturating_sub(previous.retained_bytes);
            remove_key(&mut cache.order, &path);
        }
        cache.retained_bytes = cache.retained_bytes.saturating_add(retained_bytes);
        cache.entries.insert(
            path.clone(),
            CacheEntry {
                image,
                retained_bytes,
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
    while cache.entries.len() > MAX_ENTRIES || cache.retained_bytes > MAX_RETAINED_BYTES {
        let Some(oldest) = cache.order.pop_front() else {
            break;
        };
        if let Some(entry) = cache.entries.remove(&oldest) {
            cache.retained_bytes = cache.retained_bytes.saturating_sub(entry.retained_bytes);
        }
    }
}

/// 读取 PNG signature + IHDR 尺寸，估算编码缓冲与 RGBA 解码后的共同驻留字节。
pub(crate) fn png_retained_bytes(bytes: &[u8]) -> Option<usize> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33
        || &bytes[..8] != PNG_SIGNATURE
        || u32::from_be_bytes(bytes[8..12].try_into().ok()?) != 13
        || &bytes[12..16] != b"IHDR"
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return None;
    }
    let decoded = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    let retained = usize::try_from(decoded).ok()?.checked_add(bytes.len())?;
    (retained <= MAX_RETAINED_BYTES).then_some(retained)
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
    fn cache_respects_retained_byte_limit() {
        let cache = ImageCache::new();
        cache.insert("first.png".into(), image(), MAX_RETAINED_BYTES);
        cache.insert("second.png".into(), image(), 1);

        assert!(cache.peek("first.png").is_none());
        assert!(cache.peek("second.png").is_some());
    }

    #[test]
    fn oversized_single_image_is_not_reloaded_forever() {
        let cache = ImageCache::new();
        cache.insert("huge.png".into(), image(), MAX_RETAINED_BYTES + 1);

        assert!(cache.peek("huge.png").is_none());
        assert!(cache.is_failed("huge.png"));
        assert!(!cache.begin_load("huge.png"));
    }

    #[test]
    fn png_retained_size_counts_decoded_pixels() {
        let png = png_header(10, 20);

        assert_eq!(png_retained_bytes(&png), Some(png.len() + 10 * 20 * 4));
        assert_eq!(png_retained_bytes(b"not png"), None);
    }

    #[test]
    fn png_retained_size_rejects_oversized_or_malformed_headers() {
        assert_eq!(png_retained_bytes(&png_header(u32::MAX, u32::MAX)), None);
        assert_eq!(
            png_retained_bytes(&png_header(MAX_IMAGE_DIMENSION + 1, 1)),
            None
        );

        let mut malformed = png_header(10, 20);
        malformed[11] = 12;
        assert_eq!(png_retained_bytes(&malformed), None);
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::from(*b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        // bit depth、color type、compression、filter、interlace + CRC 占位。
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png
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

    #[test]
    fn concurrent_image_loads_are_bounded_and_slots_are_reused() {
        let cache = ImageCache::new();
        for index in 0..MAX_IN_FLIGHT_LOADS {
            assert!(cache.begin_load(&format!("{index}.png")));
        }
        assert!(!cache.begin_load("overflow.png"));

        cache.fail("0.png");
        assert!(cache.begin_load("replacement.png"));
    }
}
