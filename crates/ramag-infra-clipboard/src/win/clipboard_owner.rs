//! Windows 剪贴板写入所有者窗口。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::w;

use ramag_domain::error::{DomainError, Result};
use tracing::warn;

/// `EmptyClipboard` 要求非空 HWND 才能让后续 `SetClipboardData` 成功。
/// 所有格式均立即渲染，关闭剪贴板后即可销毁该临时消息窗口。
pub(super) struct ClipboardOwner(HWND);

impl ClipboardOwner {
    pub(super) fn create() -> Result<Self> {
        // STATIC 是系统预注册窗口类，无需维护自定义 WndProc 或额外消息循环。
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Ramag Clipboard Owner"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                None,
                None,
            )
        };
        if hwnd.0 == 0 {
            Err(DomainError::Other(format!(
                "创建剪贴板所有者窗口失败：{}",
                windows::core::Error::from_win32()
            )))
        } else {
            Ok(Self(hwnd))
        }
    }

    pub(super) fn hwnd(&self) -> HWND {
        self.0
    }
}

impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.0) } {
            warn!(operation = "clipboard_owner_destroy", error = %error, "destroy clipboard owner window failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    use super::ClipboardOwner;

    #[test]
    fn temporary_owner_has_a_valid_window_handle() {
        let owner = ClipboardOwner::create().expect("create clipboard owner");
        assert!(unsafe { IsWindow(owner.hwnd()) }.as_bool());
    }
}
