//! 剪贴板工具：历史卡片流 + 搜索/筛选 + 设置。
//! 采集循环在 ramag-bin 的 App 级 spawn 中运行（独立于本视图生死）

pub mod actions;
pub mod views;

pub use actions::{SelectNextClip, SelectPrevClip};
pub use views::{ClipboardDrawer, ClipboardImageCache, ClipboardView};

use std::sync::Arc;

use gpui::{App, AppContext as _, Entity, Window};
use ramag_app::ClipboardService;
use ramag_domain::traits::{Tool, ToolMeta};

/// 创建剪贴板主视图（由 ramag-bin 注入 service 后注册进 Shell）
pub fn create_clipboard_view(
    service: Arc<ClipboardService>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ClipboardView> {
    cx.new(|cx| ClipboardView::new(service, window, cx))
}

/// 创建底部悬浮抽屉视图（由 ramag-bin 在 PopUp 窗口内装载）。
/// activation_target 为唤起时的平台激活标识，用于粘贴后恢复原窗口
pub fn create_clipboard_drawer(
    service: Arc<ClipboardService>,
    activation_target: Option<String>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ClipboardDrawer> {
    cx.new(|cx| ClipboardDrawer::new(service, activation_target, window, cx))
}

/// 使用跨窗口缓存创建底部悬浮抽屉，避免每次唤醒重新解码缩略图与应用图标。
pub fn create_clipboard_drawer_with_cache(
    service: Arc<ClipboardService>,
    activation_target: Option<String>,
    image_cache: ClipboardImageCache,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ClipboardDrawer> {
    cx.new(|cx| {
        ClipboardDrawer::with_image_cache(service, activation_target, image_cache, window, cx)
    })
}

pub struct ClipboardTool {
    meta: ToolMeta,
}

impl ClipboardTool {
    pub const ID: &'static str = "clipboard";

    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(
                Self::ID,
                "剪贴板",
                "剪贴历史：搜索 / 筛选 / 快速粘贴，全本地加密",
            )
            .with_icon("clipboard"),
        }
    }
}

impl Default for ClipboardTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ClipboardTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}
