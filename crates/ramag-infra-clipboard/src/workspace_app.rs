//! NSWorkspace FFI：前台应用信息 + 应用图标 PNG（来源标注）+ 打开链接 / Finder 显示

use cocoa::base::{id, nil};
use cocoa::foundation::NSArray;
use objc::{class, msg_send, sel, sel_impl};
use tracing::warn;

use ramag_domain::entities::{ClipSource, is_safe_http_url};
use ramag_domain::error::{DomainError, Result};

use crate::pasteboard::{ns_string, tiff_to_png, to_rust_string};

const MAX_FINDER_SELECTION: usize = 256;

pub(crate) fn frontmost_app() -> Option<ClipSource> {
    unsafe {
        let ws: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: id = msg_send![ws, frontmostApplication];
        if app == nil {
            return None;
        }
        let bundle_id = to_rust_string(msg_send![app, bundleIdentifier])?;
        let name =
            to_rust_string(msg_send![app, localizedName]).unwrap_or_else(|| bundle_id.clone());
        Some(ClipSource { bundle_id, name })
    }
}

/// 按 bundle_id 取应用图标并转 PNG。app 未安装 / 转码失败返回 None
pub(crate) fn app_icon_png(bundle_id: &str) -> Option<Vec<u8>> {
    unsafe {
        let ws: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url: id = msg_send![ws, URLForApplicationWithBundleIdentifier: ns_string(bundle_id)];
        if url == nil {
            return None;
        }
        let path: id = msg_send![url, path];
        let icon: id = msg_send![ws, iconForFile: path];
        if icon == nil {
            return None;
        }
        let tiff: id = msg_send![icon, TIFFRepresentation];
        let (png, _dims) = tiff_to_png(tiff)?;
        Some(png)
    }
}

/// 用默认浏览器打开链接（NSWorkspace openURL）
pub(crate) fn open_url(url: &str) -> Result<()> {
    let url = url.trim();
    if !is_safe_http_url(url) {
        return Err(DomainError::InvalidConfig(
            "仅支持不含空白或控制字符的 HTTP/HTTPS 链接".into(),
        ));
    }
    unsafe {
        let ns_url: id = msg_send![class!(NSURL), URLWithString: ns_string(url)];
        if ns_url == nil {
            return Err(DomainError::InvalidConfig("无效 HTTP/HTTPS 链接".into()));
        }
        let ws: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let ok: bool = msg_send![ws, openURL: ns_url];
        if ok {
            Ok(())
        } else {
            Err(DomainError::Other("系统未能打开该链接".into()))
        }
    }
}

/// 在 Finder 中高亮显示文件（activateFileViewerSelectingURLs，多文件一并选中）
pub(crate) fn reveal_in_finder(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let requested_count = paths.len();
    let selected = limited_finder_paths(paths)?;
    unsafe {
        let mut urls = Vec::with_capacity(selected.len());
        for path in selected {
            let url: id = msg_send![class!(NSURL), fileURLWithPath: ns_string(path)];
            if url == nil {
                return Err(DomainError::InvalidConfig(format!(
                    "无法解析 Finder 文件路径：{path}"
                )));
            }
            urls.push(url);
        }
        let arr = NSArray::arrayWithObjects(nil, &urls);
        let ws: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: () = msg_send![ws, activateFileViewerSelectingURLs: arr];
    }
    if selected.len() < requested_count {
        Err(DomainError::Other(format!(
            "文件较多，已在 Finder 中显示前 {} 个（最多 {MAX_FINDER_SELECTION} 个项目）",
            selected.len()
        )))
    } else {
        Ok(())
    }
}

fn limited_finder_paths(paths: &[String]) -> Result<&[String]> {
    let selected = &paths[..paths.len().min(MAX_FINDER_SELECTION)];
    if selected.iter().any(|path| path.contains('\0')) {
        return Err(DomainError::InvalidConfig(
            "文件路径包含 NUL 字符，无法在 Finder 中显示".into(),
        ));
    }
    if selected.len() < paths.len() {
        warn!(
            operation = "clipboard_finder_selection_limit",
            count = paths.len(),
            shown = selected.len(),
            "limit finder selection for clipboard file list"
        );
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_selection_is_bounded() {
        let paths: Vec<String> = (0..(MAX_FINDER_SELECTION + 1))
            .map(|index| format!("/tmp/{index}"))
            .collect();

        assert_eq!(
            limited_finder_paths(&paths).unwrap().len(),
            MAX_FINDER_SELECTION
        );
    }

    #[test]
    fn finder_selection_rejects_nul_path() {
        let paths = vec!["/tmp/a\0b".to_string()];

        assert!(limited_finder_paths(&paths).is_err());
    }
}
