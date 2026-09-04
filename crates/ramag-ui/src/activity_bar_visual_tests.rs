//! 活动栏的 headless 布局验收，覆盖动态工具列表的低高度窗口边界。

use std::sync::Arc;

use gpui::{TestAppContext, VisualTestContext, px, size};
use ramag_app::ToolRegistry;
use ramag_domain::{Tool, ToolMeta};

use super::ActivityBar;

struct TestTool {
    meta: ToolMeta,
}

impl Tool for TestTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

fn registry_with_tools(count: usize) -> Arc<ToolRegistry> {
    // Build only metadata-backed tools so the test isolates activity-bar layout.
    let registry = Arc::new(ToolRegistry::new());
    for index in 0..count {
        registry.register(Arc::new(TestTool {
            meta: ToolMeta::new(
                format!("activity-test-{index}"),
                format!("Test tool {index}"),
                "",
            ),
        }));
    }
    registry
}

/// 真实活动栏视图：工具项超出可视高度时，滚动区收缩而固定入口保持可见。
#[gpui::test]
fn activity_bar_keeps_fixed_actions_visible_when_tool_list_overflows(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let registry = registry_with_tools(16);
    let (_, cx) = cx.add_window_view(move |_, cx| ActivityBar::new(registry.clone(), cx));
    let cx: &mut VisualTestContext = cx;
    cx.simulate_resize(size(px(48.0), px(220.0)));
    cx.run_until_parked();

    let bar = cx.debug_bounds("activity-bar");
    assert!(bar.is_some(), "活动栏应渲染");
    let bar = bar.unwrap_or_default();
    let scroll = cx.debug_bounds("activity-tool-scroll");
    assert!(scroll.is_some(), "工具列表滚动区应渲染");
    let scroll = scroll.unwrap_or_default();
    let add = cx.debug_bounds("activity-add-menu");
    assert!(add.is_some(), "添加入口应渲染");
    let add = add.unwrap_or_default();
    let shortcuts = cx.debug_bounds("activity-shortcuts");
    assert!(shortcuts.is_some(), "快捷键入口应渲染");
    let shortcuts = shortcuts.unwrap_or_default();
    let settings = cx.debug_bounds("activity-settings");
    assert!(settings.is_some(), "设置入口应渲染");
    let settings = settings.unwrap_or_default();
    let last_tool = cx.debug_bounds("activity-tool-activity-test-15");
    assert!(last_tool.is_some(), "工具列表末项应渲染");
    let last_tool = last_tool.unwrap_or_default();

    for fixed_action in [add, shortcuts, settings] {
        assert!(fixed_action.origin.y >= bar.origin.y);
        assert!(fixed_action.bottom() <= bar.bottom());
    }
    assert!(scroll.origin.y >= bar.origin.y);
    assert!(scroll.bottom() <= bar.bottom());
    assert!(last_tool.bottom() > scroll.bottom());
}
