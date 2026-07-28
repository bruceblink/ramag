#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Ramag 独立 GPUI 终端内核与视图。

use gpui::{App, KeyBinding, actions};

mod core;
mod keys;
mod view;

actions!(ramag_terminal, [SendTab, SendBackTab]);

/// 终端聚焦时覆盖 Root 的 Tab 焦点导航，将按键发送给远端 Shell。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, Some("Terminal")),
        KeyBinding::new("shift-tab", SendBackTab, Some("Terminal")),
    ]);
}

pub use core::{
    ClipboardRequest, RgbColor, TerminalCell, TerminalCommand, TerminalCore, TerminalCursor,
    TerminalCursorShape, TerminalError, TerminalExit, TerminalSnapshot, TerminalStyle,
};
pub use keys::{TerminalKey, TerminalModifiers, encode_key};
pub use view::TerminalView;
