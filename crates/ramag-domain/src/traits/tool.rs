//! 工具元数据；UI 渲染由 `ramag-ui` 扩展。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    /// 工具唯一标识。
    pub id: String,
    pub name: String,
    pub description: String,
    /// Lucide 图标名。
    pub icon: Option<String>,
}

impl ToolMeta {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            icon: None,
        }
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }
}

pub trait Tool: Send + Sync {
    fn meta(&self) -> &ToolMeta;
}
