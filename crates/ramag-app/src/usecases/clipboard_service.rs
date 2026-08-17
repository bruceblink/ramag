//! 剪贴板采集与历史服务。

mod capture;
mod media_ops;
mod pending_media;
mod settings;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::RwLock;
use ramag_domain::entities::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSearchResult, ClipSource, ClipboardSettings,
    MAX_CLIPBOARD_SEARCH_BYTES, classify_text, fnv1a_hash, is_safe_http_url, make_preview,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{ClipboardDriver, Storage};
use tracing::{debug, warn};

use crate::usecases::clip_thumb::{THUMB_MAX_W, make_thumbnail};
use pending_media::PendingMediaDeletes;

const SETTINGS_KEY: &str = "clipboard_settings";
/// 限制异常大的剪贴设置。
const MAX_SETTINGS_JSON_BYTES: usize = 256 * 1024;

/// 固定保留上限。
const MAX_ITEMS: u32 = 1_000_000;
const MAX_AGE_DAYS: u32 = 360;

/// 最近条目的解密缓存，受条数与正文预算限制。
const CACHE_WINDOW: usize = 500;
/// 最近条目正文预算。
const CACHE_INLINE_BYTE_BUDGET: u64 = 64 * 1024 * 1024;
/// 搜索结果正文预算。
const SEARCH_INLINE_BYTE_BUDGET: u64 = CACHE_INLINE_BYTE_BUDGET;

/// 图片解码和渲染的内存上限。
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

fn validate_search_query(query: &str) -> Result<()> {
    if query.len() > MAX_CLIPBOARD_SEARCH_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴历史搜索词超过 {MAX_CLIPBOARD_SEARCH_BYTES} bytes 上限"
        )));
    }
    Ok(())
}

/// 不触发 I/O 的采集判定结果。
#[derive(Debug, PartialEq)]
pub enum CaptureDecision {
    Skip(&'static str),
    Record { hash: String, kind: ClipKind },
}

pub struct ClipboardService {
    driver: Arc<dyn ClipboardDriver>,
    storage: Arc<dyn Storage>,
    /// 写操作版本，供视图判断是否重载。
    revision: Arc<AtomicU64>,
    cache: Arc<RwLock<Vec<Arc<ClipItem>>>>,
    /// 设置缓存，避免热键循环反复 I/O。
    capture_enabled: Arc<AtomicBool>,
    alternate_hotkey: Arc<AtomicBool>,
    /// 自动粘贴设置镜像，避免加载期间误执行。
    auto_paste: Arc<AtomicBool>,
    /// 共享设置快照。
    settings_cache: Arc<RwLock<ClipboardSettings>>,
    settings_revision: Arc<AtomicU64>,
    /// 串行设置读写，防止旧保存覆盖新值。
    settings_save_lock: Arc<futures::lock::Mutex<()>>,
    /// 串行历史与媒体写操作，避免断链媒体。
    history_mutation_lock: Arc<futures::lock::Mutex<()>>,
    /// 热键注册状态。
    hotkey_state: Arc<AtomicU8>,
    /// 设置异常时暂停采集。
    settings_degraded: Arc<AtomicBool>,
    pending_media_deletes: Arc<PendingMediaDeletes>,
}

/// 全局热键注册状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyState {
    Disabled,
    Registered,
    Failed,
}

impl HotkeyState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => HotkeyState::Registered,
            2 => HotkeyState::Failed,
            _ => HotkeyState::Disabled,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            HotkeyState::Disabled => 0,
            HotkeyState::Registered => 1,
            HotkeyState::Failed => 2,
        }
    }
}

impl ClipboardService {
    pub fn new(driver: Arc<dyn ClipboardDriver>, storage: Arc<dyn Storage>) -> Self {
        Self {
            driver,
            storage,
            revision: Arc::new(AtomicU64::new(0)),
            cache: Arc::new(RwLock::new(Vec::new())),
            // 预热后恢复用户设置。
            capture_enabled: Arc::new(AtomicBool::new(false)),
            alternate_hotkey: Arc::new(AtomicBool::new(false)),
            auto_paste: Arc::new(AtomicBool::new(true)),
            settings_cache: Arc::new(RwLock::new(ClipboardSettings::default())),
            settings_revision: Arc::new(AtomicU64::new(0)),
            settings_save_lock: Arc::new(futures::lock::Mutex::new(())),
            history_mutation_lock: Arc::new(futures::lock::Mutex::new(())),
            hotkey_state: Arc::new(AtomicU8::new(HotkeyState::Disabled.as_u8())),
            settings_degraded: Arc::new(AtomicBool::new(false)),
            pending_media_deletes: Arc::new(PendingMediaDeletes::default()),
        }
    }

    pub fn hotkey_state(&self) -> HotkeyState {
        HotkeyState::from_u8(self.hotkey_state.load(Ordering::Relaxed))
    }

    pub fn set_hotkey_state(&self, state: HotkeyState) {
        self.hotkey_state.store(state.as_u8(), Ordering::Relaxed);
    }

    pub fn driver(&self) -> &Arc<dyn ClipboardDriver> {
        &self.driver
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn preload(&self) {
        // 与采集和清空共用锁。
        let _guard = self.history_mutation_lock.lock().await;
        match self
            .storage
            .clip_list_recent_bounded(CACHE_WINDOW, CACHE_INLINE_BYTE_BUDGET)
            .await
        {
            Ok(items) => {
                let mut cache: Vec<_> = items.into_iter().map(Arc::new).collect();
                truncate_cache(&mut cache, CACHE_WINDOW, CACHE_INLINE_BYTE_BUDGET);
                *self.cache.write() = cache;
                self.bump();
            }
            Err(e) => warn!(
                operation = "clipboard_cache_preload",
                error = %e,
                "clipboard cache preload failed"
            ),
        }
    }

    pub fn cached_snapshot(&self) -> Vec<Arc<ClipItem>> {
        self.cache.read().clone()
    }

    fn cache_upsert(&self, item: ClipItem) {
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(MAX_AGE_DAYS));
        let item = Arc::new(item);
        let mut c = self.cache.write();
        c.retain(|i| i.id != item.id);
        c.insert(0, item);
        c.retain(|i| i.last_used_at >= cutoff);
        truncate_cache(&mut c, CACHE_WINDOW, CACHE_INLINE_BYTE_BUDGET);
    }

    fn cache_remove(&self, id: &ClipId) {
        self.cache.write().retain(|i| &i.id != id);
    }

    fn cache_clear(&self) {
        self.cache.write().clear();
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ClipItem>> {
        validate_search_query(query)?;
        self.storage.clip_search(query, limit).await
    }

    /// 可取消的全量搜索。
    pub async fn search_cancellable(
        &self,
        query: &str,
        limit: usize,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ClipSearchResult> {
        validate_search_query(query)?;
        self.storage
            .clip_search_cancellable_bounded(query, limit, SEARCH_INLINE_BYTE_BUDGET, cancelled)
            .await
    }

    pub async fn copy_to_clipboard(&self, item: &ClipItem) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        let current = self.current_clip(&item.id).await?;
        self.write_clipboard_payload(&current, false).await?;
        self.touch_current_clip(current).await
    }

    async fn current_clip(&self, id: &ClipId) -> Result<ClipItem> {
        self.storage.clip_get(id).await?.ok_or_else(|| {
            DomainError::NotFound("该剪贴记录已被删除或清空，请刷新列表后重试".into())
        })
    }

    async fn write_clipboard_payload(&self, item: &ClipItem, plain_text: bool) -> Result<()> {
        match item.kind {
            ClipKind::Image => {
                if let Some(png) = self.load_image(item).await? {
                    self.driver.write_image_png(&png)?;
                }
            }
            ClipKind::Files => {
                if !self.driver.paths_exist(&item.files) {
                    return Err(DomainError::NotFound("文件已移动或删除".into()));
                }
                self.driver.write_files(&item.files)?;
            }
            _ => {
                if let Some(text) = &item.text {
                    let rtf = (!plain_text).then_some(item.rtf.as_deref()).flatten();
                    self.driver.write_text(text, rtf)?;
                }
            }
        }
        Ok(())
    }

    async fn touch_current_clip(&self, item: ClipItem) -> Result<()> {
        let latest = touch_item(&item, Utc::now());
        self.storage.clip_save(&latest).await?;
        self.cache_upsert(latest);
        self.bump();
        Ok(())
    }

    /// 复制并尝试粘贴到目标应用。
    pub async fn paste_to_app(
        &self,
        item: &ClipItem,
        activation_target: Option<&str>,
    ) -> Result<()> {
        self.copy_to_clipboard(item).await?;
        self.driver.paste_to_app(activation_target)
    }

    /// 仅复制纯文本。
    pub async fn copy_as_plain_text(&self, item: &ClipItem) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        let current = self.current_clip(&item.id).await?;
        self.write_clipboard_payload(&current, true).await?;
        // 仅本次按纯文本复制，保留 RTF。
        self.touch_current_clip(current).await
    }

    pub fn app_icon(&self, bundle_id: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        self.driver.app_icon_png(bundle_id)
    }

    pub fn open_url(&self, url: &str) -> Result<()> {
        let url = url.trim();
        if !is_safe_http_url(url) {
            return Err(DomainError::InvalidConfig(
                "仅支持不超过 16 KiB、且不含空白或控制字符的 HTTP/HTTPS 链接".into(),
            ));
        }
        self.driver.open_url(url)
    }

    pub fn reveal_in_file_manager(&self, paths: &[String]) -> Result<()> {
        self.driver.reveal_in_file_manager(paths)
    }
}

fn parse_clipboard_settings(json: &str) -> Result<ClipboardSettings> {
    if json.len() > MAX_SETTINGS_JSON_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴设置数据过大：{} bytes",
            json.len()
        )));
    }
    let settings = serde_json::from_str::<ClipboardSettings>(json)
        .map_err(|e| DomainError::InvalidConfig(format!("剪贴设置格式无效：{e}")))?;
    settings.validate().map_err(DomainError::InvalidConfig)?;
    Ok(settings)
}

fn serialize_clipboard_settings(settings: &ClipboardSettings) -> Result<String> {
    settings.validate().map_err(DomainError::InvalidConfig)?;
    let json = serde_json::to_string(settings)
        .map_err(|e| DomainError::Storage(format!("序列化剪贴设置失败：{e}")))?;
    if json.len() > MAX_SETTINGS_JSON_BYTES {
        return Err(DomainError::InvalidConfig(format!(
            "剪贴设置数据过大：{} bytes",
            json.len()
        )));
    }
    Ok(json)
}

/// 判断是否记录本次采集，不执行 I/O。
pub fn decide_capture(captured: &CapturedClip, settings: &ClipboardSettings) -> CaptureDecision {
    if captured.concealed {
        return CaptureDecision::Skip("concealed");
    }

    // 与驱动读取优先级一致。
    if !captured.files.is_empty() {
        if file_payload_len(&captured.files) > settings.max_item_bytes {
            return CaptureDecision::Skip("files too large");
        }
        return CaptureDecision::Record {
            hash: format!("{:016x}", file_payload_hash(&captured.files)),
            kind: ClipKind::Files,
        };
    }
    if let Some(png) = &captured.image_png {
        if (png.len() as u64) > settings.max_item_bytes {
            return CaptureDecision::Skip("image too large");
        }
        let Some((width, height)) = captured.image_dims else {
            return CaptureDecision::Skip("invalid image");
        };
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if width == 0
            || height == 0
            || width > MAX_IMAGE_DIMENSION
            || height > MAX_IMAGE_DIMENSION
            || pixels > MAX_IMAGE_PIXELS
        {
            return CaptureDecision::Skip("image dimensions too large");
        }
        if !settings.capture_images {
            return CaptureDecision::Skip("image capture disabled");
        }
        return CaptureDecision::Record {
            hash: hash_hex(png),
            kind: ClipKind::Image,
        };
    }
    if let Some(text) = &captured.text {
        if text.trim().is_empty() {
            return CaptureDecision::Skip("empty text");
        }
        let total_bytes = text
            .len()
            .saturating_add(captured.rtf.as_ref().map_or(0, Vec::len));
        if (total_bytes as u64) > settings.max_item_bytes {
            return CaptureDecision::Skip("text too large");
        }
        return CaptureDecision::Record {
            hash: hash_hex(text.as_bytes()),
            kind: classify_text(text),
        };
    }
    CaptureDecision::Skip("empty")
}

fn hash_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a_hash(bytes))
}

fn truncate_cache(cache: &mut Vec<Arc<ClipItem>>, max_items: usize, max_inline_bytes: u64) {
    cache.truncate(max_items);
    if max_inline_bytes == 0 {
        cache.clear();
        return;
    }
    let mut total = 0u64;
    let mut keep = 0usize;
    for item in cache.iter() {
        let next = total.saturating_add(item.inline_payload_bytes());
        if keep > 0 && next > max_inline_bytes {
            break;
        }
        total = next;
        keep += 1;
        if total >= max_inline_bytes {
            break;
        }
    }
    cache.truncate(keep);
}

fn file_payload_len(files: &[String]) -> u64 {
    files.iter().fold(
        u64::try_from(files.len().saturating_sub(1)).unwrap_or(u64::MAX),
        |total, path| total.saturating_add(u64::try_from(path.len()).unwrap_or(u64::MAX)),
    )
}

fn file_payload_hash(files: &[String]) -> u64 {
    let mut hash = FNV1A_OFFSET;
    for (index, path) in files.iter().enumerate() {
        if index > 0 {
            update_fnv1a(&mut hash, std::iter::once(b'\n'));
        }
        update_fnv1a(&mut hash, path.bytes());
    }
    hash
}

fn inline_payload_matches(existing: &ClipItem, captured: &CapturedClip, kind: ClipKind) -> bool {
    match kind {
        ClipKind::Files => existing.files == captured.files,
        ClipKind::Text | ClipKind::Link | ClipKind::Color => existing.text == captured.text,
        ClipKind::Image => false,
    }
}

/// 主指纹碰撞时用反向 FNV 组合，兼容既有 16 位哈希。
fn collision_hash(captured: &CapturedClip, primary: &str) -> String {
    let secondary = if !captured.files.is_empty() {
        reverse_file_payload_hash(&captured.files)
    } else if let Some(png) = &captured.image_png {
        reverse_fnv1a_hash(png)
    } else if let Some(text) = &captured.text {
        reverse_fnv1a_hash(text.as_bytes())
    } else {
        reverse_fnv1a_hash(&[])
    };
    format!("{primary}-{secondary:016x}")
}

const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

fn update_fnv1a(hash: &mut u64, bytes: impl Iterator<Item = u8>) {
    for byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV1A_PRIME);
    }
}

fn reverse_file_payload_hash(files: &[String]) -> u64 {
    let mut hash = FNV1A_OFFSET;
    for (index, path) in files.iter().rev().enumerate() {
        if index > 0 {
            update_fnv1a(&mut hash, std::iter::once(b'\n'));
        }
        update_fnv1a(&mut hash, path.bytes().rev());
    }
    hash
}

fn reverse_fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_OFFSET;
    update_fnv1a(&mut hash, bytes.iter().rev().copied());
    hash
}

/// 更新使用时间，不改原始负载。
fn touch_item(item: &ClipItem, now: chrono::DateTime<Utc>) -> ClipItem {
    let mut latest = item.clone();
    latest.last_used_at = now;
    latest
}

/// 在线程池生成缩略图。
async fn make_thumbnail_off_thread(png: Arc<Vec<u8>>) -> Result<Vec<u8>> {
    crate::run_blocking(move || make_thumbnail(png.as_slice(), THUMB_MAX_W)).await
}

#[cfg(test)]
mod tests;
