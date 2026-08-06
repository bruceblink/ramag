//! 应用更新领域实体。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// GitHub Release 中可下载的单个平台安装包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
    /// 小写十六进制 SHA-256；API 未提供时由下载驱动读取校验清单。
    pub sha256: Option<String>,
}

/// 已发布的稳定版本及其安装包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag_name: String,
    pub release_url: String,
    pub notes: String,
    pub published_at: Option<String>,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

pub type UpdateProgressFn = Arc<dyn Fn(DownloadProgress) + Send + Sync>;

/// 下载取消句柄；驱动在每个数据块之间检查，避免留下可安装的半成品。
#[derive(Clone, Default)]
pub struct UpdateCancellation(Arc<AtomicBool>);

impl UpdateCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for UpdateCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
