//! 本机系统监控与任务管理器工具。

mod monitor;
mod view;

pub use monitor::{
    DiskSnapshot, HISTORY_SECONDS, MAX_VISIBLE_PROCESSES, MonitorSnapshot, ProcessSnapshot,
    ProcessSort, RefreshInterval, SystemMonitor, TerminateResult,
};
pub use view::SystemView;

use gpui::{App, AppContext as _, Entity, Window};
use ramag_domain::traits::{Tool, ToolMeta};

/// 在主窗口中创建系统工具视图；采集工作由视图内部的后台任务执行。
pub fn create_system_view(window: &mut Window, cx: &mut App) -> Entity<SystemView> {
    cx.new(|cx_inner| SystemView::new(window, cx_inner))
}

/// 系统工具在 Ramag 工具注册表中的元数据。
pub struct SystemTool {
    meta: ToolMeta,
}

impl SystemTool {
    pub const ID: &'static str = "system";

    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(Self::ID, "系统监控", "查看本机性能、磁盘、网络和运行中进程")
                .with_icon("gauge"),
        }
    }
}

impl Default for SystemTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for SystemTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata_exposes_system_entry() {
        let tool = SystemTool::new();
        assert_eq!(tool.meta().id, SystemTool::ID);
        assert_eq!(tool.meta().name, "系统监控");
        assert!(tool.meta().icon.is_some());
    }
}
