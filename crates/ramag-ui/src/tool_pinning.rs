use gpui::{App, Global};

use ramag_app::ToolRegistry;

/// 首页固定工具的全局快照；工具注册表负责校验 ID，UI 只保存已清理结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPinningGlobal(pub Vec<String>);

impl Global for ToolPinningGlobal {}

/// 读取当前固定工具顺序，缺少初始化时返回空列表。
pub fn pinned_tools(cx: &App) -> Vec<String> {
    cx.try_global::<ToolPinningGlobal>()
        .map(|state| state.0.clone())
        .unwrap_or_default()
}

/// 设置固定工具顺序，并在状态确实变化时通知主页重绘。
pub fn set_pinned_tools(pinned: Vec<String>, cx: &mut App) {
    if cx
        .try_global::<ToolPinningGlobal>()
        .is_some_and(|current| current.0 == pinned)
    {
        return;
    }
    cx.set_global(ToolPinningGlobal(pinned));
}

/// 切换已注册工具的固定状态并异步保存最新顺序。
pub fn toggle_tool_pinned(id: &str, registry: &ToolRegistry, cx: &mut App) {
    let current = pinned_tools(cx);
    let Some(next) = registry.toggle_pinned(id, &current) else {
        return;
    };
    set_pinned_tools(next.clone(), cx);
    if let Ok(value) = serde_json::to_string(&next) {
        crate::preferences::persist_preference_latest(ramag_app::TOOL_PINNED_PREF_KEY, value, cx);
    }
}

/// 将固定工具移动到固定区目标位置，并返回是否发生变化。
pub fn reorder_pinned(id: &str, target_index: usize, cx: &mut App) {
    let mut pinned = pinned_tools(cx);
    let Some(source_index) = pinned.iter().position(|item| item == id) else {
        return;
    };
    let item = pinned.remove(source_index);
    pinned.insert(target_index.min(pinned.len()), item);
    set_pinned_tools(pinned.clone(), cx);
    if let Ok(value) = serde_json::to_string(&pinned) {
        crate::preferences::persist_preference_latest(ramag_app::TOOL_PINNED_PREF_KEY, value, cx);
    }
}

/// 从固定列表移除工具并保存最新固定状态。
pub fn unpin_tool(id: &str, cx: &mut App) {
    let mut pinned = pinned_tools(cx);
    let before = pinned.len();
    pinned.retain(|item| item != id);
    if pinned.len() == before {
        return;
    }
    set_pinned_tools(pinned.clone(), cx);
    if let Ok(value) = serde_json::to_string(&pinned) {
        crate::preferences::persist_preference_latest(ramag_app::TOOL_PINNED_PREF_KEY, value, cx);
    }
}

/// 解析启动偏好、过滤无效 ID，并初始化 UI 全局状态；返回是否丢弃了无效数据。
pub fn init_tool_pinning(
    preference: Option<&str>,
    registry: &ToolRegistry,
    cx: &mut App,
) -> Result<bool, String> {
    let Some(raw) = preference else {
        set_pinned_tools(Vec::new(), cx);
        return Ok(false);
    };
    let pinned = serde_json::from_str::<Vec<String>>(raw)
        .map_err(|error| format!("固定工具偏好格式无效：{error}"))?;
    let (sanitized, changed) = registry.apply_pinned(&pinned);
    set_pinned_tools(sanitized, cx);
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ramag_app::ToolRegistry;
    use ramag_domain::{Tool, ToolMeta};

    use super::init_tool_pinning;

    struct DummyTool(ToolMeta);

    impl Tool for DummyTool {
        fn meta(&self) -> &ToolMeta {
            &self.0
        }
    }

    #[gpui::test]
    fn startup_pinning_uses_only_registered_tools(cx: &mut gpui::TestAppContext) {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool(ToolMeta::new("a", "A", ""))));
        cx.update(
            |app| match init_tool_pinning(Some(r#"["a","missing","a"]"#), &registry, app) {
                Ok(changed) => {
                    assert!(changed);
                    assert_eq!(super::pinned_tools(app), ["a"]);
                }
                Err(error) => {
                    assert_eq!(error, "unreachable");
                }
            },
        )
    }

    #[gpui::test]
    fn invalid_startup_pinning_is_reported(cx: &mut gpui::TestAppContext) {
        let registry = ToolRegistry::new();
        cx.update(|app| assert!(init_tool_pinning(Some("not-json"), &registry, app).is_err()));
    }
}
