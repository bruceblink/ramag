#![allow(unexpected_cfgs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 剪贴板驱动：按平台分发。
//! - macOS：NSPasteboard / NSWorkspace / CGEvent / Carbon 全局热键（见 `macos` 模块）
//! - Windows：Win32 剪贴板 / 全局热键 / 模拟粘贴（见 `win` 模块；支持 PNG 与 CF_DIB）
//!
//! 对外统一导出 `PlatformClipboardDriver` 与 `HotkeyListener`，调用方无需写平台分支。

#[cfg(any(target_os = "windows", test))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[path = "win/dib.rs"]
mod dib;
mod media;

#[cfg(target_os = "macos")]
mod hotkey;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod paste;
#[cfg(target_os = "macos")]
mod pasteboard;
#[cfg(target_os = "macos")]
mod workspace_app;

#[cfg(target_os = "macos")]
pub use hotkey::HotkeyListener;
#[cfg(target_os = "macos")]
pub use macos::MacClipboardDriver as PlatformClipboardDriver;
#[cfg(target_os = "macos")]
pub fn foreground_display_index() -> Option<usize> {
    None
}

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
pub use win::{
    HotkeyListener, WinClipboardDriver as PlatformClipboardDriver, foreground_display_index,
};
