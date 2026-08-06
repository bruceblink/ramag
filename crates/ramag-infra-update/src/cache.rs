//! 更新缓存路径与符号链接边界校验。

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use ramag_domain::error::{DomainError, Result};
use semver::Version;

pub(super) fn update_cache_dir(version: &str) -> Result<PathBuf> {
    Version::parse(version)
        .map_err(|error| DomainError::InvalidConfig(format!("更新缓存版本无效：{error}")))?;
    ProjectDirs::from("com", "axemc", "Ramag")
        .map(|dirs| dirs.cache_dir().join("updates").join(version))
        .ok_or_else(|| DomainError::Storage("无法定位应用更新缓存目录".into()))
}

pub(super) async fn reject_symlink(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::Storage(format!(
            "更新缓存路径不能是符号链接：{}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage(format!(
            "检查更新缓存路径失败 {}：{error}",
            path.display()
        ))),
    }
}
