//! 剪贴板采集与历史服务。

mod media_ops;
mod pending_media;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::RwLock;
use ramag_domain::entities::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSearchResult, ClipSource, ClipboardSettings,
    MAX_CLIPBOARD_SEARCH_BYTES, blacklist_matches, classify_text, fnv1a_hash, is_safe_http_url,
    make_preview,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{ClipboardDriver, Storage};
use tracing::{debug, warn};

use crate::usecases::clip_thumb::{THUMB_MAX_W, make_thumbnail};
use pending_media::PendingMediaDeletes;

const SETTINGS_KEY: &str = "clipboard_settings";
/// 剪贴设置远小于通用偏好 16 MiB 上限；先拒绝异常大 JSON，避免反序列化时无谓分配。
const MAX_SETTINGS_JSON_BYTES: usize = 256 * 1024;

/// 历史清理上限（固定策略，不开放设置）：最多 100 万条 / 360 天，超出在每次入库后清理最旧
const MAX_ITEMS: u32 = 1_000_000;
const MAX_AGE_DAYS: u32 = 360;

/// 内存缓存窗口：常驻最近 N 条（已解密），视图唤起 / 刷新同步读；内存与历史总量解耦。
/// 取 500 而非上万：启动只解密这些条；快照通过 Arc 共享正文，不再深拷贝大文本；
/// 更早的历史由主视图与抽屉的全量存储搜索（`search`）覆盖，不靠缓存兜底。
/// 与 SEARCH_LIMIT 同量级，避免"缓存即时层"与"全量层"结果规模悬殊
const CACHE_WINDOW: usize = 500;
/// 文本、RTF 与文件路径在内存窗口中的总预算；最新一条即使较大也保留，避免记录刚产生就不可见。
const CACHE_INLINE_BYTE_BUDGET: u64 = 64 * 1024 * 1024;
/// 后台搜索结果与缓存使用相同正文预算，避免匹配大量大文本时再次放大内存。
const SEARCH_INLINE_BYTE_BUDGET: u64 = CACHE_INLINE_BYTE_BUDGET;

/// 防止高压缩图片在解码/渲染时膨胀为超大内存。
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

/// 不触发 IO 的采集判定结果。
#[derive(Debug, PartialEq)]
pub enum CaptureDecision {
    Skip(&'static str),
    Record { hash: String, kind: ClipKind },
}

pub struct ClipboardService {
    driver: Arc<dyn ClipboardDriver>,
    storage: Arc<dyn Storage>,
    /// 写操作后自增，视图仅在变化时重载解密。
    revision: Arc<AtomicU64>,
    cache: Arc<RwLock<Vec<Arc<ClipItem>>>>,
    /// 避免热键循环每拍读取并解密设置。
    capture_enabled: Arc<AtomicBool>,
    alternate_hotkey: Arc<AtomicBool>,
    /// 自动粘贴设置镜像。抽屉构造时同步读取，避免异步加载设置期间误执行自动粘贴。
    auto_paste: Arc<AtomicBool>,
    /// 多个设置入口共享同一快照。
    settings_cache: Arc<RwLock<ClipboardSettings>>,
    settings_revision: Arc<AtomicU64>,
    /// 设置读写串行化，避免慢读取或旧保存晚完成后覆盖新值。
    settings_save_lock: Arc<futures::lock::Mutex<()>>,
    /// 历史与媒体写操作串行化，避免采集、清空、删除和复制提升相互穿插后留下断链媒体。
    history_mutation_lock: Arc<futures::lock::Mutex<()>>,
    /// 热键循环写、设置面板读，使注册失败对用户可见。
    hotkey_state: Arc<AtomicU8>,
    /// 设置异常时暂停采集并向用户告警。
    settings_degraded: Arc<AtomicBool>,
    pending_media_deletes: Arc<PendingMediaDeletes>,
}

/// 全局热键注册状态（AtomicU8 编码）
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
            // 首次初始化默认关闭；启动由 prime_capture_enabled 恢复用户已保存的选择
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
        // 与采集/清空共用锁，避免慢预热最后用旧快照覆盖启动期间刚写入的缓存。
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
            Err(e) => warn!(error = %e, "clipboard cache preload failed"),
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

    /// 读取失败或损坏时暂停采集；仅无已存设置时采用默认值。
    pub async fn load_settings(&self) -> ClipboardSettings {
        // 与保存共用串行锁：防止早先发起的慢读取在新设置保存后才回包，
        // 又把内存快照覆盖回旧值。
        let _guard = self.settings_save_lock.lock().await;
        let settings = match self.storage.get_preference(SETTINGS_KEY).await {
            Ok(Some(json)) => match parse_clipboard_settings(&json) {
                Ok(s) => {
                    self.settings_degraded.store(false, Ordering::Relaxed);
                    s
                }
                Err(e) => {
                    warn!(error = %e, "clipboard settings corrupted; capture disabled");
                    self.settings_degraded.store(true, Ordering::Relaxed);
                    ClipboardSettings {
                        enabled: false,
                        ..ClipboardSettings::default()
                    }
                }
            },
            Ok(None) => {
                self.settings_degraded.store(false, Ordering::Relaxed);
                ClipboardSettings::default()
            }
            Err(e) => {
                warn!(error = %e, "clipboard settings unreadable; capture disabled");
                self.settings_degraded.store(true, Ordering::Relaxed);
                ClipboardSettings {
                    enabled: false,
                    ..ClipboardSettings::default()
                }
            }
        };
        // 与读取同处串行区间内同步运行时镜像，避免启动预热在新保存之后
        // 才把旧值写回内存。
        self.capture_enabled
            .store(settings.enabled, Ordering::Relaxed);
        self.alternate_hotkey
            .store(settings.alternate_hotkey, Ordering::Relaxed);
        self.auto_paste
            .store(settings.auto_paste, Ordering::Relaxed);
        self.cache_settings(&settings);
        settings
    }

    pub fn settings_degraded(&self) -> bool {
        self.settings_degraded.load(Ordering::Relaxed)
    }

    pub async fn save_settings(&self, settings: &ClipboardSettings) -> Result<()> {
        let json = serialize_clipboard_settings(settings)?;
        let _guard = self.settings_save_lock.lock().await;
        // 先更新内存镜像（与 UI 乐观更新一致），热键循环最迟下一拍生效
        let prev_enabled = self
            .capture_enabled
            .swap(settings.enabled, Ordering::Relaxed);
        let prev_alternate = self
            .alternate_hotkey
            .swap(settings.alternate_hotkey, Ordering::Relaxed);
        let prev_auto_paste = self.auto_paste.swap(settings.auto_paste, Ordering::Relaxed);
        let result = self.storage.set_preference(SETTINGS_KEY, &json).await;
        // 持久化失败回滚镜像：内存与落盘不一致会让「界面已关但仍在采集」类偏差跨拍存在
        if result.is_err() {
            self.capture_enabled.store(prev_enabled, Ordering::Relaxed);
            self.alternate_hotkey
                .store(prev_alternate, Ordering::Relaxed);
            self.auto_paste.store(prev_auto_paste, Ordering::Relaxed);
        } else {
            self.cache_settings(settings);
        }
        result
    }

    fn cache_settings(&self, settings: &ClipboardSettings) {
        let mut cached = self.settings_cache.write();
        if &*cached != settings {
            *cached = settings.clone();
            self.settings_revision.fetch_add(1, Ordering::Release);
        }
    }

    /// 一致读取快照与版本号。若恰好遇到更新，重试而不返回“旧快照 + 新版本”。
    pub fn settings_snapshot_with_revision(&self) -> (ClipboardSettings, u64) {
        loop {
            let before = self.settings_revision.load(Ordering::Acquire);
            let snapshot = self.settings_cache.read().clone();
            let after = self.settings_revision.load(Ordering::Acquire);
            if before == after {
                return (snapshot, after);
            }
        }
    }

    /// 等待在途保存后读取一致的内存快照。
    pub async fn capture_settings_snapshot(&self) -> ClipboardSettings {
        let _guard = self.settings_save_lock.lock().await;
        self.settings_cache.read().clone()
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    pub fn capture_enabled(&self) -> bool {
        self.capture_enabled.load(Ordering::Relaxed)
    }

    pub fn alternate_hotkey(&self) -> bool {
        self.alternate_hotkey.load(Ordering::Relaxed)
    }

    pub fn auto_paste(&self) -> bool {
        self.auto_paste.load(Ordering::Relaxed)
    }

    pub async fn prime_capture_enabled(&self) -> bool {
        self.load_settings().await.enabled
    }

    pub async fn capture_tick(&self, settings: &ClipboardSettings) -> Result<bool> {
        if !settings.enabled {
            return Ok(false);
        }
        let count = self.driver.change_count();
        // 自写回产生的变更跳过（避免复制回剪贴板又记一遍）
        if count == self.driver.own_change_count() {
            return Ok(false);
        }
        let Some(captured) = self.driver.read()? else {
            return Ok(false);
        };
        let source = self.driver.frontmost_app();

        match decide_capture(&captured, settings, source.as_ref()) {
            CaptureDecision::Skip(reason) => {
                debug!(reason, "clipboard capture skipped");
                Ok(false)
            }
            CaptureDecision::Record { hash, kind } => {
                let _guard = self.history_mutation_lock.lock().await;
                self.record(captured, kind, hash, source, settings).await
            }
        }
    }

    async fn record(
        &self,
        mut captured: CapturedClip,
        kind: ClipKind,
        hash: String,
        source: Option<ClipSource>,
        settings: &ClipboardSettings,
    ) -> Result<bool> {
        let now = Utc::now();
        let primary_hash = hash;
        let mut content_hash = primary_hash.clone();
        if let Some(mut existing) = self.storage.clip_find_by_hash(&content_hash).await? {
            if self.payload_matches(&existing, &captured, kind).await {
                existing.last_used_at = now;
                if let Some(src) = source.clone() {
                    existing.source = Some(src);
                }
                self.storage.clip_save(&existing).await?;
                self.cache_upsert(existing);
                self.bump();
                return Ok(true);
            }

            warn!(
                clip_id = %existing.id,
                hash = %content_hash,
                "clipboard content hash collision detected"
            );
            content_hash = collision_hash(&captured, &primary_hash);
        }

        // 同一碰撞内容后续仍命中自己的二级哈希，避免每次复制都新增一条。
        if content_hash != primary_hash
            && let Some(mut existing) = self.storage.clip_find_by_hash(&content_hash).await?
            && self.payload_matches(&existing, &captured, kind).await
        {
            existing.last_used_at = now;
            if let Some(src) = source.clone() {
                existing.source = Some(src);
            }
            self.storage.clip_save(&existing).await?;
            self.cache_upsert(existing);
            self.bump();
            return Ok(true);
        }

        // 图片先完成受限解码，拒绝只有伪造 PNG 头的损坏输入；再把原图与缩略图加密落盘。
        // 缩略图生成（解码 + 缩放 + 编码）是 CPU 大头，挪工作线程避免采集时 UI 卡顿
        let image_png = captured.image_png.take().map(Arc::new);
        let (image_path, thumb_path) = match (&image_png, settings.capture_images) {
            (Some(png), true) => {
                let thumb = match make_thumbnail_off_thread(png.clone()).await {
                    Ok(thumb) => thumb,
                    Err(error) => {
                        warn!(error = %error, "invalid clipboard image ignored");
                        return Ok(false);
                    }
                };
                let enc_full = self.storage.seal(png.as_slice()).await?;
                let full = self
                    .persist_media(format!("{content_hash}.img"), enc_full)
                    .await?;
                let thumb_result = async {
                    let enc_thumb = self.storage.seal(&thumb).await?;
                    self.persist_media(format!("{content_hash}.thumb"), enc_thumb)
                        .await
                }
                .await;
                let thumb_path = match thumb_result {
                    Ok(path) => path,
                    Err(error) => {
                        let rollback = self.unprotected_staged_media(vec![full.clone()]);
                        if let Err(cleanup_error) = self.cleanup_media(rollback).await {
                            warn!(
                                error = %cleanup_error,
                                path = %full,
                                stage = "thumbnail",
                                "rollback clipboard image failed"
                            );
                        }
                        return Err(error);
                    }
                };
                (Some(full), Some(thumb_path))
            }
            _ => (None, None),
        };
        let byte_size = if let Some(png) = &image_png {
            png.len() as u64
        } else if let Some(text) = &captured.text {
            text.len()
                .saturating_add(captured.rtf.as_ref().map_or(0, Vec::len)) as u64
        } else {
            captured.files.iter().map(String::len).sum::<usize>() as u64
        };
        let preview = make_preview(
            kind,
            captured.text.as_deref(),
            &captured.files,
            captured.image_dims,
        );

        let item = ClipItem {
            id: ClipId::new(),
            kind,
            text: captured.text,
            rtf: captured.rtf,
            image_path,
            thumb_path,
            image_dims: captured.image_dims,
            files: captured.files,
            preview,
            source,
            byte_size,
            content_hash,
            created_at: now,
            last_used_at: now,
        };
        if let Err(error) = self.storage.clip_save(&item).await {
            let staged_media = self.unprotected_staged_media(
                [&item.image_path, &item.thumb_path]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect(),
            );
            if let Err(cleanup_error) = self.cleanup_media(staged_media).await {
                warn!(
                    error = %cleanup_error,
                    clip_id = %item.id,
                    stage = "record_save",
                    "rollback clipboard media failed"
                );
            }
            return Err(error);
        }
        self.protect_item_media(&item);
        self.cache_upsert(item);
        self.prune().await;
        self.bump();
        Ok(true)
    }

    async fn payload_matches(
        &self,
        existing: &ClipItem,
        captured: &CapturedClip,
        kind: ClipKind,
    ) -> bool {
        if existing.kind != kind {
            return false;
        }
        if !matches!(kind, ClipKind::Image) {
            return inline_payload_matches(existing, captured, kind);
        }

        let Some(expected) = captured.image_png.as_deref() else {
            return false;
        };
        if existing.byte_size != expected.len() as u64 || existing.image_dims != captured.image_dims
        {
            return false;
        }
        match self.load_image(existing).await {
            Ok(Some(actual)) => actual == expected,
            Ok(None) => false,
            Err(error) => {
                warn!(error = %error, clip_id = %existing.id, "verify clipboard image hash failed");
                false
            }
        }
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ClipItem>> {
        validate_search_query(query)?;
        self.storage.clip_search(query, limit).await
    }

    /// 可取消的全量搜索；视图输入变化后用取消标记尽快停止旧扫描。
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
                // 文件失效校验：路径已不存在则拒绝复制，提示用户
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
        // 复制即提升为最新
        let latest = touch_item(&item, Utc::now());
        self.storage.clip_save(&latest).await?;
        self.cache_upsert(latest);
        self.bump();
        Ok(())
    }

    /// 复制并粘贴到目标应用（需辅助功能权限；无权限降级为仅复制并返回 Err）
    pub async fn paste_to_app(
        &self,
        item: &ClipItem,
        activation_target: Option<&str>,
    ) -> Result<()> {
        self.copy_to_clipboard(item).await?;
        self.driver.paste_to_app(activation_target)
    }

    /// 仅复制纯文本（剥离 RTF 富文本格式）；非文本类型回退普通复制
    pub async fn copy_as_plain_text(&self, item: &ClipItem) -> Result<()> {
        let _guard = self.history_mutation_lock.lock().await;
        let current = self.current_clip(&item.id).await?;
        self.write_clipboard_payload(&current, true).await?;
        // “纯文本”只影响本次写回；历史中的 RTF 必须保留，否则之后普通复制
        // 无法再恢复原富文本内容，属于不可见的数据损失。
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

/// 纯判定：是否记录该次采集（无 IO，便于测试）
pub fn decide_capture(
    captured: &CapturedClip,
    settings: &ClipboardSettings,
    source: Option<&ClipSource>,
) -> CaptureDecision {
    if captured.concealed {
        return CaptureDecision::Skip("concealed");
    }
    if let Some(src) = source
        && settings
            .blacklist
            .iter()
            .any(|blocked| blacklist_matches(blocked, &src.bundle_id))
    {
        return CaptureDecision::Skip("blacklisted");
    }

    // 文件优先，其次图片，最后文本（与驱动读取优先级一致）
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

/// 仅在主 FNV 指纹真实碰撞时使用；反向 FNV 与主哈希组合，兼容既有 16 位哈希记录。
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

/// 提升条目最近使用时间，但不改写任何原始负载（text / RTF / 媒体引用）。
fn touch_item(item: &ClipItem, now: chrono::DateTime<Utc>) -> ClipItem {
    let mut latest = item.clone();
    latest.last_used_at = now;
    latest
}

/// 工作线程生成缩略图（std::thread + oneshot，与 Storage 桥接同款；不引入 runtime）。
/// 采集循环跑在 GPUI 前台 executor，图片编解码留在主线程会造成可感知卡顿
async fn make_thumbnail_off_thread(png: Arc<Vec<u8>>) -> Result<Vec<u8>> {
    crate::run_blocking(move || make_thumbnail(png.as_slice(), THUMB_MAX_W)).await
}

#[cfg(test)]
mod tests;
