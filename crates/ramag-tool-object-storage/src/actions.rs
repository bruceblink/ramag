use gpui::Action;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = ramag_object_storage)]
pub struct RefreshObjectStorage;
