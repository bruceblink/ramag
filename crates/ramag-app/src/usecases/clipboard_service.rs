//! ClipboardService：剪贴板采集 + 历史聚合。与 ConnectionService 并列，共用同一份 redb。
//! 采集判定（去重 / 黑名单 / 大小 / 分类）抽成纯函数 `decide_capture` 便于测试，
//! `capture_tick` 仅做 driver/storage 编排

mod media_ops;
mod pending_media;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use chrono::Utc;
use parking_lot::RwLock;
use ramag_domain::entities::{
    CapturedClip, ClipId, ClipItem, ClipKind, ClipSource, ClipboardSettings, blacklist_matches,
    classify_text, fnv1a_hash, make_preview,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{ClipboardDriver, Storage};
use tracing::{debug, warn};

use crate::usecases::clip_thumb::{THUMB_MAX_W, make_thumbnail};
use pending_media::PendingMediaDeletes;

/// 设置持久化 key（prefs 表，JSON）
const SETTINGS_KEY: &str = "clipboard_settings";

/// 历史清理上限（固定策略，不开放设置）：最多 100 万条 / 360 天，超出在每次入库后清理最旧
const MAX_ITEMS: u32 = 1_000_000;
const MAX_AGE_DAYS: u32 = 360;

/// 内存缓存窗口：常驻最近 N 条（已解密），视图唤起 / 刷新同步读；内存与历史总量解耦。
/// 取 500 而非上万：启动只解密这些条、每次快照 / 过滤只深拷贝这些条（成本随窗口线性）；
/// 更早的历史由主视图与抽屉的全量存储搜索（`search`）覆盖，不靠缓存兜底。
/// 与 SEARCH_LIMIT 同量级，避免"缓存即时层"与"全量层"结果规模悬殊
const CACHE_WINDOW: usize = 500;

/// 防止高压缩图片在解码/渲染时膨胀为超大内存。
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// 采集判定结果（纯逻辑产物，不触 IO）
#[derive(Debug, PartialEq)]
pub enum CaptureDecision {
    /// 跳过：隐私标记 / 黑名单 / 超限 / 空内容
    Skip(&'static str),
    /// 内容指纹（用于查重）+ 待入库条目骨架（image_path 由编排层补）
    Record { hash: String, kind: ClipKind },
}

pub struct ClipboardService {
    driver: Arc<dyn ClipboardDriver>,
    storage: Arc<dyn Storage>,
    /// 历史变更版本号：任何写操作（采集 / 复制 / 删除）后自增。
    /// 视图轮询此值，仅在变化时才重载解密，避免每拍全表解密
    revision: Arc<AtomicU64>,
    /// 已解密的最近 N 条窗口缓存（最近优先）。写操作增量维护，视图同步快照取
    cache: Arc<RwLock<Vec<ClipItem>>>,
    /// 采集开关内存镜像：设置读取/保存时在同一串行区间内同步。
    /// 供 App 级热键循环每拍读，避免每拍解密设置；关采集即据此释放全局热键
    capture_enabled: Arc<AtomicBool>,
    /// 备用热键内存镜像（主修饰键+Alt+V），维护方式同 capture_enabled。
    alternate_hotkey: Arc<AtomicBool>,
    /// 自动粘贴设置镜像。抽屉构造时同步读取，避免异步加载设置期间误执行自动粘贴。
    auto_paste: Arc<AtomicBool>,
    /// 设置快照与版本号：多个设置入口共享同一真实状态，避免各自长期持有过期副本。
    settings_cache: Arc<RwLock<ClipboardSettings>>,
    settings_revision: Arc<AtomicU64>,
    /// 设置读写串行化，避免慢读取或旧保存晚完成后覆盖新值。
    settings_save_lock: Arc<futures::lock::Mutex<()>>,
    /// 全局热键注册状态（见 HotkeyState）：App 级热键循环写，设置面板读，
    /// 让「热键没注册上（如被占用）」对用户可见而非只留日志
    hotkey_state: Arc<AtomicU8>,
    /// 设置降级标记：读取失败 / JSON 损坏时置位（采集 fail-closed），设置面板显示告警
    settings_degraded: Arc<AtomicBool>,
    /// 图片删除的待清理媒体；存在即仍在撤销窗口内。
    pending_media_deletes: Arc<PendingMediaDeletes>,
}

/// 全局热键注册状态（AtomicU8 编码）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyState {
    /// 采集关闭，未注册（主动释放）
    Disabled,
    /// 已注册，热键可用
    Registered,
    /// 注册失败（热键被其它应用占用等）
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
            // 默认开（同 ClipboardSettings::default）；启动由 prime_capture_enabled 校正
            capture_enabled: Arc::new(AtomicBool::new(true)),
            alternate_hotkey: Arc::new(AtomicBool::new(false)),
            auto_paste: Arc::new(AtomicBool::new(true)),
            settings_cache: Arc::new(RwLock::new(ClipboardSettings::default())),
            settings_revision: Arc::new(AtomicU64::new(0)),
            settings_save_lock: Arc::new(futures::lock::Mutex::new(())),
            hotkey_state: Arc::new(AtomicU8::new(HotkeyState::Disabled.as_u8())),
            settings_degraded: Arc::new(AtomicBool::new(false)),
            pending_media_deletes: Arc::new(PendingMediaDeletes::default()),
        }
    }

    /// 当前全局热键注册状态（设置面板展示用）
    pub fn hotkey_state(&self) -> HotkeyState {
        HotkeyState::from_u8(self.hotkey_state.load(Ordering::Relaxed))
    }

    /// 热键循环在注册 / 注销 / 失败时上报状态
    pub fn set_hotkey_state(&self, state: HotkeyState) {
        self.hotkey_state.store(state.as_u8(), Ordering::Relaxed);
    }

    pub fn driver(&self) -> &Arc<dyn ClipboardDriver> {
        &self.driver
    }

    /// 当前历史版本号（视图据此判断是否需要重载）
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    // —— 内存窗口缓存 ——

    /// 启动预热：解密最近 CACHE_WINDOW 条入缓存（仅启动调一次）
    pub async fn preload(&self) {
        match self.storage.clip_list_recent(CACHE_WINDOW).await {
            Ok(items) => {
                *self.cache.write() = items;
                self.bump();
            }
            Err(e) => warn!(error = %e, "clip cache preload failed"),
        }
    }

    /// 同步取缓存快照（已解密、最近优先）。视图唤起 / 刷新用，无 IO、无解密
    pub fn cached_snapshot(&self) -> Vec<ClipItem> {
        self.cache.read().clone()
    }

    /// 缓存增量更新：移除旧同 id → 插最前 → 去超龄 + 截窗口（全内存，不解密）
    fn cache_upsert(&self, item: ClipItem) {
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(MAX_AGE_DAYS));
        let mut c = self.cache.write();
        c.retain(|i| i.id != item.id);
        c.insert(0, item);
        c.retain(|i| i.last_used_at >= cutoff);
        if c.len() > CACHE_WINDOW {
            c.truncate(CACHE_WINDOW);
        }
    }

    fn cache_remove(&self, id: &ClipId) {
        self.cache.write().retain(|i| &i.id != id);
    }

    fn cache_clear(&self) {
        self.cache.write().clear();
    }

    // —— 设置 ——

    /// 读取设置。隐私 fail-closed：存储读取失败或 JSON 损坏时**默认关闭采集**并置
    /// 降级标记（设置面板显示告警），绝不因异常回退到「继续记录」；仅「从未保存过」
    /// 才用出厂默认（enabled=true，首次使用语义）
    pub async fn load_settings(&self) -> ClipboardSettings {
        // 与保存共用串行锁：防止早先发起的慢读取在新设置保存后才回包，
        // 又把内存快照覆盖回旧值。
        let _guard = self.settings_save_lock.lock().await;
        let settings = match self.storage.get_preference(SETTINGS_KEY).await {
            Ok(Some(json)) => match serde_json::from_str(&json) {
                Ok(s) => {
                    self.settings_degraded.store(false, Ordering::Relaxed);
                    s
                }
                Err(e) => {
                    warn!(error = %e, "clip settings corrupted; capture fail-closed");
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
                warn!(error = %e, "clip settings unreadable; capture fail-closed");
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

    /// 设置是否处于降级态（读取失败 / 损坏，采集已 fail-closed 暂停）
    pub fn settings_degraded(&self) -> bool {
        self.settings_degraded.load(Ordering::Relaxed)
    }

    pub async fn save_settings(&self, settings: &ClipboardSettings) -> Result<()> {
        let _guard = self.settings_save_lock.lock().await;
        // 先更新内存镜像（与 UI 乐观更新一致），热键循环最迟下一拍生效
        let prev_enabled = self
            .capture_enabled
            .swap(settings.enabled, Ordering::Relaxed);
        let prev_alternate = self
            .alternate_hotkey
            .swap(settings.alternate_hotkey, Ordering::Relaxed);
        let prev_auto_paste = self.auto_paste.swap(settings.auto_paste, Ordering::Relaxed);
        let result = async {
            let json = serde_json::to_string(settings)
                .map_err(|e| DomainError::Storage(format!("序列化剪贴设置失败：{e}")))?;
            self.storage.set_preference(SETTINGS_KEY, &json).await
        }
        .await;
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

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    /// 采集是否开启（内存镜像，热键循环每拍读）
    pub fn capture_enabled(&self) -> bool {
        self.capture_enabled.load(Ordering::Relaxed)
    }

    /// 是否使用备用热键组合（内存镜像，热键循环每拍读）
    pub fn alternate_hotkey(&self) -> bool {
        self.alternate_hotkey.load(Ordering::Relaxed)
    }

    /// 抽屉当前是否应在复制后自动粘贴（启动预热与设置保存都会同步更新）。
    pub fn auto_paste(&self) -> bool {
        self.auto_paste.load(Ordering::Relaxed)
    }

    /// 启动时预热持久化设置并返回采集开关。`load_settings` 已在串行区间内
    /// 同步全部内存镜像，这里不再二次写入，避免启动期覆盖用户刚保存的新值。
    pub async fn prime_capture_enabled(&self) -> bool {
        self.load_settings().await.enabled
    }

    // —— 采集 ——

    /// 轮询一拍：changeCount 变化时读取并按设置决定是否入库。
    /// 返回 true 表示历史有变更（UI 需刷新）
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
                debug!(reason, "clip capture skipped");
                Ok(false)
            }
            CaptureDecision::Record { hash, kind } => {
                self.record(captured, kind, hash, source, settings).await
            }
        }
    }

    /// 入库：命中指纹则提升旧条目，否则新建（图片先落盘）
    async fn record(
        &self,
        captured: CapturedClip,
        kind: ClipKind,
        hash: String,
        source: Option<ClipSource>,
        settings: &ClipboardSettings,
    ) -> Result<bool> {
        let now = Utc::now();
        if let Some(mut existing) = self.storage.clip_find_by_hash(&hash).await? {
            existing.last_used_at = now;
            if let Some(src) = source {
                existing.source = Some(src);
            }
            self.storage.clip_save(&existing).await?;
            self.cache_upsert(existing);
            self.bump();
            return Ok(true);
        }

        // 图片先完成受限解码，拒绝只有伪造 PNG 头的损坏输入；再把原图与缩略图加密落盘。
        // 缩略图生成（解码 + 缩放 + 编码）是 CPU 大头，挪工作线程避免采集时 UI 卡顿
        let (image_path, thumb_path) = match (&captured.image_png, settings.capture_images) {
            (Some(png), true) => {
                let thumb = match make_thumbnail_off_thread(png.clone()).await {
                    Ok(thumb) => thumb,
                    Err(error) => {
                        warn!(error = %error, "invalid clipboard image ignored");
                        return Ok(false);
                    }
                };
                let enc_full = self.storage.seal(png).await?;
                let full = self
                    .driver
                    .persist_media(&format!("{hash}.img"), &enc_full)?;
                let enc_thumb = self.storage.seal(&thumb).await?;
                let thumb_path = self
                    .driver
                    .persist_media(&format!("{hash}.thumb"), &enc_thumb)?;
                (Some(full), Some(thumb_path))
            }
            _ => (None, None),
        };
        let byte_size = if let Some(png) = &captured.image_png {
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
            content_hash: hash,
            created_at: now,
            last_used_at: now,
        };
        self.storage.clip_save(&item).await?;
        self.cache_upsert(item);
        self.prune().await;
        self.bump();
        Ok(true)
    }

    // —— 历史读取 / 操作 ——

    /// 全量搜索（覆盖缓存窗口之外的历史）。主视图后台去抖调用，匹配 preview/text
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ClipItem>> {
        self.storage.clip_search(query, limit).await
    }

    /// 复制条目回剪贴板（不自动粘贴）
    pub async fn copy_to_clipboard(&self, item: &ClipItem) -> Result<()> {
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
                    self.driver.write_text(text, item.rtf.as_deref())?;
                }
            }
        }
        // 复制即提升为最新
        let mut latest = item.clone();
        latest.last_used_at = Utc::now();
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
        match &item.text {
            Some(text) => {
                self.driver.write_text(text, None)?;
                let mut latest = item.clone();
                latest.rtf = None;
                latest.last_used_at = Utc::now();
                self.storage.clip_save(&latest).await?;
                self.cache_upsert(latest);
                self.bump();
                Ok(())
            }
            None => self.copy_to_clipboard(item).await,
        }
    }

    /// 来源应用图标 PNG（按 bundle_id 缓存）；卡片右上角显示用
    pub fn app_icon(&self, bundle_id: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        self.driver.app_icon_png(bundle_id)
    }

    /// 用默认浏览器打开链接
    pub fn open_url(&self, url: &str) -> Result<()> {
        self.driver.open_url(url)
    }

    /// 在系统文件管理器中显示文件
    pub fn reveal_in_file_manager(&self, paths: &[String]) -> Result<()> {
        self.driver.reveal_in_file_manager(paths)
    }
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
        let joined = captured.files.join("\n");
        if (joined.len() as u64) > settings.max_item_bytes {
            return CaptureDecision::Skip("files too large");
        }
        return CaptureDecision::Record {
            hash: hash_hex(joined.as_bytes()),
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

/// 工作线程生成缩略图（std::thread + oneshot，与 Storage 桥接同款；不引入 runtime）。
/// 采集循环跑在 GPUI 前台 executor，图片编解码留在主线程会造成可感知卡顿
async fn make_thumbnail_off_thread(png: Vec<u8>) -> Result<Vec<u8>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("ramag-clip-thumb".into())
        .spawn(move || {
            let _ = tx.send(make_thumbnail(&png, THUMB_MAX_W));
        })
        .map_err(|e| DomainError::Other(format!("启动缩略图线程失败：{e}")))?;
    rx.await
        .map_err(|_| DomainError::Other("缩略图线程中断".into()))?
}

#[cfg(test)]
mod tests;
