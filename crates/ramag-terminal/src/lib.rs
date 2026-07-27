#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Ramag 独立 GPUI 终端内核与视图。

mod core;
mod keys;
mod view;

pub use core::{
    ClipboardRequest, RgbColor, TerminalCell, TerminalCommand, TerminalCore, TerminalCursor,
    TerminalCursorShape, TerminalError, TerminalExit, TerminalSnapshot, TerminalStyle,
};
pub use keys::{TerminalKey, TerminalModifiers, encode_key};
pub use view::TerminalView;
