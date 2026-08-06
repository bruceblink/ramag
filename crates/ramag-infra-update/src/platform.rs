//! 使用系统文件管理器显示已校验的安装包。

use std::path::Path;
use std::process::Command;

use ramag_domain::error::{DomainError, Result};

pub(super) fn reveal_download(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        DomainError::InvalidConfig(format!("安装包路径缺少父目录：{}", path.display()))
    })?;
    if !path.is_file() {
        return Err(DomainError::NotFound(format!(
            "已下载安装包不存在：{}",
            path.display()
        )));
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer");
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command.arg(parent).spawn().map_err(|error| {
        DomainError::Other(format!("打开安装包目录失败 {}：{error}", parent.display()))
    })?;
    Ok(())
}
