#![allow(unexpected_cfgs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! 剪贴板驱动：按平台分发。
//! - macOS：NSPasteboard / NSWorkspace / CGEvent / Carbon 全局热键（见 `macos` 模块）
//! - Windows：Win32 剪贴板 / 全局热键 / 模拟粘贴（见 `win` 模块；图片走注册 PNG 格式，采集截图的 CF_DIB 待补）
//!
//! 对外统一导出 `PlatformClipboardDriver` 与 `HotkeyListener`，调用方无需写平台分支。

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

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
pub use win::{HotkeyListener, WinClipboardDriver as PlatformClipboardDriver};
