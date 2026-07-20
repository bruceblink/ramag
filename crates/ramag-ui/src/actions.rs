//! 跨工具共用 Action

use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

/// 主修饰键+W 关 Tab。各 Tool Session 先消费，没消费则冒泡到 main.rs 关窗
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct CloseTab;

/// 主修饰键+1/2/3 跳到第 N 个已注册工具（Shell 处理）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct SelectTool1;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct SelectTool2;
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct SelectTool3;

/// Ctrl+Tab 在主区各区段（首页 + 工具 + 设置）间循环切换（Shell 处理）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct CycleSection;

/// Ctrl+Shift+Tab 反向循环主区区段。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct CycleSectionReverse;
