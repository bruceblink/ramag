//! Windows 剪贴板驱动：整合 clipboard / app / hotkey / media。
//! 结构对称 macOS 侧（见 `macos` 模块）：写操作后记录序列号供自写回抑制。

mod app;
mod clipboard;
mod clipboard_owner;
mod hotkey;
mod shell;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

pub use hotkey::HotkeyListener;

pub fn foreground_display_index() -> Option<usize> {
    app::foreground_display_index()
}

use parking_lot::Mutex;
use ramag_domain::entities::{CapturedClip, ClipSource};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::ClipboardDriver;

use crate::media;

pub struct WinClipboardDriver {
    /// 最近一次本应用写回产生的序列号，采集循环据此跳过自写回
    own_change: AtomicI64,
    media: media::MediaStore,
    icon_cache: Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>,
    last_source: Mutex<Option<ClipSource>>,
}

impl WinClipboardDriver {
    pub fn new() -> Self {
        Self {
            own_change: AtomicI64::new(-1),
            media: media::MediaStore::new(),
            icon_cache: Mutex::new(HashMap::new()),
            last_source: Mutex::new(None),
        }
    }

    /// 记录写入期间取得的精确序列号，避免关闭剪贴板后的竞争窗口。
    fn mark_own_write(&self, sequence: i64) {
        self.own_change.store(sequence, Ordering::Relaxed);
    }
}

impl Default for WinClipboardDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardDriver for WinClipboardDriver {
    fn change_count(&self) -> i64 {
        clipboard::sequence_number()
    }

    fn own_change_count(&self) -> i64 {
        self.own_change.load(Ordering::Relaxed)
    }

    fn read(&self) -> Result<Option<CapturedClip>> {
        let Some(read) = clipboard::read()? else {
            *self.last_source.lock() = None;
            return Ok(None);
        };
        let source =
            app::app_for_window_handle(read.owner_hwnd, read.owner_pid).or_else(app::frontmost_app);
        *self.last_source.lock() = source;
        Ok(Some(read.clip))
    }

    fn write_text(&self, text: &str, rtf: Option<&[u8]>) -> Result<()> {
        let sequence = clipboard::write_text(text, rtf)?;
        self.mark_own_write(sequence);
        Ok(())
    }

    fn write_image_png(&self, png: &[u8]) -> Result<()> {
        let sequence = clipboard::write_image_png(png)?;
        self.mark_own_write(sequence);
        Ok(())
    }

    fn write_files(&self, paths: &[String]) -> Result<()> {
        let sequence = clipboard::write_files(paths)?;
        self.mark_own_write(sequence);
        Ok(())
    }

    fn frontmost_app(&self) -> Option<ClipSource> {
        self.last_source.lock().clone()
    }

    fn activation_target(&self) -> Option<String> {
        app::foreground_window_token()
    }

    fn app_icon_png(&self, bundle_id: &str) -> Option<Arc<Vec<u8>>> {
        let mut cache = self.icon_cache.lock();
        if let Some(hit) = cache.get(bundle_id) {
            return hit.clone();
        }
        let icon = app::app_icon_png(bundle_id).map(Arc::new);
        cache.insert(bundle_id.to_string(), icon.clone());
        icon
    }

    fn persist_media(&self, key: &str, bytes: &[u8]) -> Result<String> {
        self.media.persist(key, bytes)
    }

    fn read_media(&self, path: &str) -> Result<Vec<u8>> {
        self.media.read(path)
    }

    fn list_media(&self) -> Result<Vec<String>> {
        self.media.list()
    }

    fn remove_media(&self, path: &str) -> Result<()> {
        self.media.remove(path)
    }

    fn accessibility_trusted(&self, _prompt: bool) -> bool {
        // Windows 的 SendInput 无需辅助功能授权
        true
    }

    fn paste_to_app(&self, activation_target: Option<&str>) -> Result<()> {
        let target = activation_target
            .ok_or_else(|| DomainError::NotFound("未记录到原窗口，内容已复制到剪贴板".into()))?;
        app::activate_window_token(target)?;
        app::post_ctrl_v_delayed(180, target)
    }

    fn open_url(&self, url: &str) -> Result<()> {
        shell::open_url(url)
    }

    fn reveal_in_file_manager(&self, paths: &[String]) -> Result<()> {
        shell::reveal_in_explorer(paths)
    }

    fn paths_exist(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|p| std::path::Path::new(p).exists())
    }
}
