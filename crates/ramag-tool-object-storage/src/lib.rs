#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 云对象存储工具 UI。

mod views;

use std::sync::Arc;

use gpui::{App, AppContext as _, Entity, Window};
use ramag_app::ObjectStorageService;
use ramag_domain::traits::{Tool, ToolMeta};

pub use views::ObjectStorageView;

pub fn create_object_storage_view(
    service: Arc<ObjectStorageService>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ObjectStorageView> {
    cx.new(|cx| ObjectStorageView::new(service, window, cx))
}

pub struct ObjectStorageTool {
    meta: ToolMeta,
}

impl ObjectStorageTool {
    pub const ID: &'static str = "object_storage";

    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(Self::ID, "云存储", "统一管理腾讯云 COS 与阿里云 OSS")
                .with_icon("cloud"),
        }
    }
}

impl Default for ObjectStorageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ObjectStorageTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata_exposes_object_storage_entry() {
        let tool = ObjectStorageTool::new();
        assert_eq!(tool.meta().id, "object_storage");
        assert!(tool.meta().icon.is_some());
    }
}
