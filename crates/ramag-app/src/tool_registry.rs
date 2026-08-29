//! 工具注册表，支持按用户布局顺序查询和动态隐藏工具入口。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use ramag_domain::Tool;

/// 工具入口顺序在 Storage 中使用的偏好键。
pub const TOOL_ORDER_PREF_KEY: &str = "tool_order";

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
            tracing::warn!(
                operation = "tool_register",
                tool_id = %tool.meta().id,
                reason = "duplicate",
                "duplicate tool registration ignored"
            );
            return;
        }
        tracing::info!(
            operation = "tool_register",
            tool_id = %tool.meta().id,
            name = %tool.meta().name,
            "tool registered"
        );
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
        tracing::info!(
            operation = "tool_visibility_update",
            tool_id = %id,
            enabled,
            "tool visibility changed"
        );
        true
    }

    /// 按当前布局顺序返回已启用的工具。
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .read()
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.tool.clone())
            .collect()
    }

    /// 返回全部工具的当前顺序，包含暂时隐藏的工具以兼容平台差异。
    pub fn order(&self) -> Vec<String> {
        self.tools
            .read()
            .iter()
            .map(|entry| entry.tool.meta().id.clone())
            .collect()
    }

    /// 将启动时读取的 JSON 顺序应用到注册表；未知 ID 会被忽略。
    pub fn apply_order_json(&self, json: &str) -> Result<bool, serde_json::Error> {
        let order = serde_json::from_str::<Vec<String>>(json)?;
        Ok(self.apply_order(&order))
    }

    /// 按偏好中的 ID 排序，同时保留未出现在偏好中的新工具及其注册顺序。
    pub fn apply_order(&self, order: &[String]) -> bool {
        if order.is_empty() {
            return false;
        }

        let ranks: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        let mut tools = self.tools.write();
        let previous = tools
            .iter()
            .map(|entry| entry.tool.meta().id.clone())
            .collect::<Vec<_>>();
        tools.sort_by_key(|entry| {
            ranks
                .get(entry.tool.meta().id.as_str())
                .copied()
                .unwrap_or(order.len())
        });
        let changed = previous
            != tools
                .iter()
                .map(|entry| entry.tool.meta().id.clone())
                .collect::<Vec<_>>();
        if changed {
            tracing::info!(operation = "tool_order_load", "tool layout restored");
        }
        changed
    }

    /// 将一个可见工具插入另一个可见工具之前或之后，并返回顺序是否改变。
    pub fn reorder(&self, dragged_id: &str, target_id: &str, before: bool) -> bool {
        if dragged_id == target_id {
            return false;
        }

        let mut tools = self.tools.write();
        let Some(dragged_index) = tools
            .iter()
            .position(|entry| entry.enabled && entry.tool.meta().id == dragged_id)
        else {
            return false;
        };
        if !tools
            .iter()
            .any(|entry| entry.enabled && entry.tool.meta().id == target_id)
        {
            return false;
        }

        let previous_order = tools
            .iter()
            .map(|entry| entry.tool.meta().id.clone())
            .collect::<Vec<_>>();
        let dragged = tools.remove(dragged_index);
        let Some(target_index) = tools
            .iter()
            .position(|entry| entry.enabled && entry.tool.meta().id == target_id)
        else {
            let restore_index = dragged_index.min(tools.len());
            tools.insert(restore_index, dragged);
            return false;
        };
        let insert_index = if before {
            target_index
        } else {
            target_index + 1
        };
        let insert_index = insert_index.min(tools.len());
        tools.insert(insert_index, dragged);
        let changed = previous_order
            != tools
                .iter()
                .map(|entry| entry.tool.meta().id.clone())
                .collect::<Vec<_>>();
        if !changed {
            return false;
        }
        tracing::info!(
            operation = "tool_order_update",
            dragged_id,
            target_id,
            before,
            "tool layout changed"
        );
        true
    }

    /// 将工具移动到目标工具当前所在的位置，适合整项拖拽而不是按半区插入。
    pub fn reorder_to_target(&self, dragged_id: &str, target_id: &str) -> bool {
        if dragged_id == target_id {
            return false;
        }

        let (dragged_index, target_index) = {
            let tools = self.tools.read();
            let Some(dragged_index) = tools
                .iter()
                .position(|entry| entry.enabled && entry.tool.meta().id == dragged_id)
            else {
                return false;
            };
            let Some(target_index) = tools
                .iter()
                .position(|entry| entry.enabled && entry.tool.meta().id == target_id)
            else {
                return false;
            };
            (dragged_index, target_index)
        };

        // Keep the dragged item in the target's original slot: insert after the
        // target when it started before it, and before it when it started after it.
        self.reorder(dragged_id, target_id, dragged_index > target_index)
    }

    /// 将可见工具移动到当前所有可见工具的末尾，供末尾整项落点使用。
    pub fn move_to_end(&self, dragged_id: &str) -> bool {
        let target_id = {
            let tools = self.tools.read();
            tools
                .iter()
                .rev()
                .find(|entry| entry.enabled)
                .map(|entry| entry.tool.meta().id.clone())
        };
        target_id.is_some_and(|target_id| self.reorder(dragged_id, &target_id, false))
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

    #[test]
    fn reorder_updates_visible_tool_order() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));
        reg.register(dummy("c", "ToolC"));

        assert!(reg.reorder("c", "a", true));
        assert_eq!(reg.order(), ["c", "a", "b"]);
        assert!(reg.reorder("c", "b", false));
        assert_eq!(reg.order(), ["a", "b", "c"]);
        assert!(!reg.reorder("a", "missing", true));
    }

    #[test]
    fn reorder_to_target_moves_item_into_target_slot() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));
        reg.register(dummy("c", "ToolC"));
        reg.register(dummy("d", "ToolD"));

        assert!(reg.reorder_to_target("a", "c"));
        assert_eq!(reg.order(), ["b", "c", "a", "d"]);
        assert!(reg.reorder_to_target("d", "b"));
        assert_eq!(reg.order(), ["d", "b", "c", "a"]);
        assert!(!reg.reorder_to_target("a", "a"));
    }

    #[test]
    fn move_to_end_places_item_after_last_visible_tool() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));
        reg.register(dummy("c", "ToolC"));

        assert!(reg.move_to_end("b"));
        assert_eq!(reg.order(), ["a", "c", "b"]);
        assert!(!reg.move_to_end("b"));
    }

    #[test]
    fn apply_order_keeps_new_tools_after_saved_tools() {
        let reg = ToolRegistry::new();
        reg.register(dummy("a", "ToolA"));
        reg.register(dummy("b", "ToolB"));
        reg.register(dummy("c", "ToolC"));

        assert!(reg.apply_order(&["b".into(), "a".into()]));
        assert_eq!(reg.order(), ["b", "a", "c"]);
        assert!(reg.apply_order_json(r#"["c","a"]"#).unwrap());
        assert_eq!(reg.order(), ["c", "a", "b"]);
        assert!(reg.apply_order_json("not-json").is_err());
    }
}
