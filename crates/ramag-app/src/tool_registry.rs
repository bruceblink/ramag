//! 工具注册表，支持按注册顺序查询和动态隐藏工具入口。

use std::sync::Arc;

use parking_lot::RwLock;
use ramag_domain::Tool;

struct ToolEntry {
    tool: Arc<dyn Tool>,
    enabled: bool,
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<Vec<ToolEntry>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        let mut tools = self.tools.write();
        if tools.iter().any(|t| t.tool.meta().id == tool.meta().id) {
            tracing::warn!(tool_id = %tool.meta().id, "duplicate tool registration ignored");
            return;
        }
        tracing::info!(tool_id = %tool.meta().id, name = %tool.meta().name, "tool registered");
        tools.push(ToolEntry {
            tool,
            enabled: true,
        });
    }

    /// 设置工具入口可见性，返回状态是否变化；未注册时返回 `false`。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut tools = self.tools.write();
        let Some(entry) = tools.iter_mut().find(|t| t.tool.meta().id == id) else {
            return false;
        };
        if entry.enabled == enabled {
            return false;
        }
        entry.enabled = enabled;
        tracing::info!(tool_id = %id, enabled, "tool visibility changed");
        true
    }

    /// 按注册顺序返回已启用的工具。
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .read()
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.tool.clone())
            .collect()
    }

    pub fn find(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .iter()
            .find(|t| t.enabled && t.tool.meta().id == id)
            .map(|t| t.tool.clone())
    }

    pub fn count(&self) -> usize {
        self.tools.read().iter().filter(|t| t.enabled).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ramag_domain::ToolMeta;

    struct DummyTool {
        meta: ToolMeta,
    }

    impl Tool for DummyTool {
        fn meta(&self) -> &ToolMeta {
            &self.meta
        }
    }

    fn dummy(id: &str, name: &str) -> Arc<DummyTool> {
        Arc::new(DummyTool {
            meta: ToolMeta::new(id, name, ""),
        })
    }

    #[test]
    fn register_and_list() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));
        assert_eq!(reg.count(), 2);
        assert!(reg.find("a").is_some());
        assert!(reg.find("missing").is_none());
    }

    #[test]
    fn duplicate_registration_ignored() {
        let reg = ToolRegistry::new();
        reg.register(dummy("dup", "Tool1"));
        reg.register(dummy("dup", "Tool2"));
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn disabled_tool_hidden_from_list_and_find() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));

        assert!(reg.set_enabled("a", false));
        assert_eq!(reg.count(), 1);
        assert!(reg.find("a").is_none());
        assert_eq!(reg.list().len(), 1);
        // 重复设置同一状态与未注册 id 均不算变化
        assert!(!reg.set_enabled("a", false));
        assert!(!reg.set_enabled("missing", true));

        assert!(reg.set_enabled("a", true));
        assert!(reg.find("a").is_some());
        assert_eq!(reg.count(), 2);
    }
}
