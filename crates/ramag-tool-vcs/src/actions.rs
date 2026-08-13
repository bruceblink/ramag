//! VCS 快捷键 Action。绑定在 ramag-bin/main.rs 的 `cx.bind_keys`（context = "VcsView"）

use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

/// 提交暂存区（默认主修饰键+Enter；仅提交输入框聚焦时生效）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_vcs)]
pub struct CommitNow;

/// Push 当前分支（默认主修饰键+Shift+K）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_vcs)]
pub struct PushNow;

/// Pull 当前分支（默认主修饰键+T）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_vcs)]
pub struct PullNow;

/// 显示 / 隐藏底部历史面板（默认主修饰键+Shift+H）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_vcs)]
pub struct ToggleHistoryPane;
