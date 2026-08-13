//! 剪贴板工具快捷键 Action。绑定在 ramag-bin/main.rs 的 `cx.bind_keys`

use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

/// 选中项下移（默认 down）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_clipboard)]
pub struct SelectNextClip;

/// 选中项上移（默认 up）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_clipboard)]
pub struct SelectPrevClip;
