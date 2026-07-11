//! 默认浏览器与 Windows 资源管理器集成。

use std::path::{Path, PathBuf};

use ramag_domain::error::{DomainError, Result};
use tracing::warn;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILFindLastID, ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::w;

use crate::win::clipboard::{pcwstr, wide_nul};

const MAX_EXPLORER_WINDOWS: usize = 8;
const MAX_SELECTED_ITEMS: usize = 256;

struct ComApartment(bool);

impl ComApartment {
    fn enter() -> Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if status.is_ok() {
            Ok(Self(true))
        } else if status == RPC_E_CHANGED_MODE {
            // 当前线程已用其它 apartment 模式初始化，仍满足 Shell API 的 COM 前置条件。
            Ok(Self(false))
        } else {
            Err(DomainError::Other(format!(
                "初始化 Windows Shell COM 失败：{}",
                windows::core::Error::from_hresult(status)
            )))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct ItemIdList(*mut ITEMIDLIST);

impl Drop for ItemIdList {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.0)) };
    }
}

struct PathGroup<'a> {
    parent: PathBuf,
    key: String,
    items: Vec<&'a str>,
}

/// 默认浏览器打开 HTTP/HTTPS 链接。
pub(super) fn open_url(url: &str) -> Result<()> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if url.chars().any(char::is_control)
        || !(lower.starts_with("http://") || lower.starts_with("https://"))
    {
        return Err(DomainError::InvalidConfig(
            "仅支持无控制字符的 HTTP/HTTPS 链接".into(),
        ));
    }
    let wide = wide_nul(url);
    let result =
        unsafe { ShellExecuteW(None, w!("open"), pcwstr(&wide), None, None, SW_SHOWNORMAL) };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(DomainError::Other(format!(
            "打开链接失败（ShellExecuteW 返回 {}）",
            result.0 as isize
        )))
    }
}

/// 同一父目录的项目在同一个 Explorer 窗口批量选中。
pub(super) fn reveal_in_explorer(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let groups = group_paths(paths)?;
    let _com = ComApartment::enter()?;
    for (index, group) in groups.iter().take(MAX_EXPLORER_WINDOWS).enumerate() {
        select_group(group, index)?;
    }

    let shown: usize = groups
        .iter()
        .take(MAX_EXPLORER_WINDOWS)
        .map(|group| group.items.len())
        .sum();
    if shown < paths.len() {
        warn!(
            count = paths.len(),
            shown, "limit explorer selection for clipboard file list"
        );
        Err(DomainError::Other(format!(
            "文件较多，已在资源管理器中显示前 {shown} 个（最多 {MAX_EXPLORER_WINDOWS} 个目录、{MAX_SELECTED_ITEMS} 个项目）"
        )))
    } else {
        Ok(())
    }
}

fn group_paths(paths: &[String]) -> Result<Vec<PathGroup<'_>>> {
    let mut groups: Vec<PathGroup<'_>> = Vec::new();
    for path in paths.iter().take(MAX_SELECTED_ITEMS) {
        if path.contains('\0') {
            return Err(DomainError::InvalidConfig(
                "文件路径包含 NUL 字符，无法打开资源管理器".into(),
            ));
        }
        let parent = Path::new(path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| DomainError::InvalidConfig("文件路径缺少父目录".into()))?;
        let key = parent.to_string_lossy().to_ascii_lowercase();
        if let Some(group) = groups.iter_mut().find(|group| group.key == key) {
            group.items.push(path);
        } else {
            groups.push(PathGroup {
                parent: parent.to_path_buf(),
                key,
                items: vec![path],
            });
        }
    }
    Ok(groups)
}

fn select_group(group: &PathGroup<'_>, group_index: usize) -> Result<()> {
    let parent = group.parent.to_string_lossy();
    let parent_pidl = create_pidl(&parent, group_index)?;
    let mut item_pidls = Vec::with_capacity(group.items.len());
    let mut children = Vec::with_capacity(group.items.len());
    for path in &group.items {
        let pidl = create_pidl(path, group_index)?;
        let child = unsafe { ILFindLastID(pidl.0) };
        if child.is_null() {
            return Err(DomainError::NotFound("无法定位资源管理器项目".into()));
        }
        children.push(child.cast_const());
        item_pidls.push(pidl);
    }
    unsafe { SHOpenFolderAndSelectItems(parent_pidl.0, Some(&children), 0) }.map_err(|error| {
        DomainError::Other(format!(
            "在资源管理器中显示第 {} 个目录失败：{error}",
            group_index + 1
        ))
    })
}

fn create_pidl(path: &str, group_index: usize) -> Result<ItemIdList> {
    let wide = wide_nul(path);
    let mut raw = std::ptr::null_mut();
    unsafe { SHParseDisplayName(pcwstr(&wide), None, &mut raw, 0, None) }.map_err(|error| {
        DomainError::NotFound(format!(
            "无法在资源管理器中定位第 {} 个目录中的项目：{error}",
            group_index + 1
        ))
    })?;
    if raw.is_null() {
        return Err(DomainError::NotFound(format!(
            "无法在资源管理器中定位第 {} 个目录中的项目",
            group_index + 1
        )));
    }
    Ok(ItemIdList(raw))
}
