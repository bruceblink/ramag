//! Windows 剪贴板驱动：整合 clipboard / app / hotkey / media。
//! 结构对称 macOS 侧（见 `macos` 模块）：写操作后记录序列号供自写回抑制。

mod app;
mod clipboard;
mod hotkey;

use std::sync::atomic::{AtomicI64, Ordering};

pub use hotkey::HotkeyListener;

use ramag_domain::entities::{CapturedClip, ClipSource};
use ramag_domain::error::Result;
use ramag_domain::traits::ClipboardDriver;

use crate::media;

pub struct WinClipboardDriver {
    /// 最近一次本应用写回产生的序列号，采集循环据此跳过自写回
    own_change: AtomicI64,
    media: media::MediaStore,
}

impl WinClipboardDriver {
    pub fn new() -> Self {
        Self {
            own_change: AtomicI64::new(-1),
            media: media::MediaStore::new(),
        }
    }

    /// 写操作后记录当前序列号（自写回抑制）
    fn mark_own_write(&self) {
        self.own_change
            .store(clipboard::sequence_number(), Ordering::Relaxed);
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
        clipboard::read()
    }

    fn write_text(&self, text: &str, rtf: Option<&[u8]>) -> Result<()> {
        clipboard::write_text(text, rtf)?;
        self.mark_own_write();
        Ok(())
    }

    fn write_image_png(&self, png: &[u8]) -> Result<()> {
        clipboard::write_image_png(png)?;
        self.mark_own_write();
        Ok(())
    }

    fn write_files(&self, paths: &[String]) -> Result<()> {
        clipboard::write_files(paths)?;
        self.mark_own_write();
        Ok(())
    }

    fn frontmost_app(&self) -> Option<ClipSource> {
        app::frontmost_app()
    }

    fn app_icon_png(&self, _bundle_id: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        // 来源应用图标提取（ExtractIconEx）待实现，暂不标注图标
        None
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

    fn paste_to_app(&self, _bundle_id: Option<&str>) -> Result<()> {
        // 抽屉先切回原应用，再延迟发 Ctrl-V
        app::post_ctrl_v_delayed(180);
        Ok(())
    }

    fn open_url(&self, url: &str) -> Result<()> {
        app::open_url(url)
    }

    fn reveal_in_finder(&self, paths: &[String]) -> Result<()> {
        app::reveal_in_explorer(paths)
    }

    fn paths_exist(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|p| std::path::Path::new(p).exists())
    }
}
