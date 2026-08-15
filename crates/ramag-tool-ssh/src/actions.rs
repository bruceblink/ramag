//! SSH 工作区快捷键 Action。

use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema, Action)]
#[action(namespace = ssh)]
pub struct NewSshTerminal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema, Action)]
#[action(namespace = ssh)]
pub struct CloseSshTerminal;
