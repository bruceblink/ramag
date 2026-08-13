use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

/// 主修饰键+W 关 Tab。各 Tool Session 先消费，没消费则冒泡到 main.rs 关窗
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct CloseTab;

/// 主修饰键+P 打开当前工具的最近项目列表。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag)]
pub struct OpenRecentItems;
