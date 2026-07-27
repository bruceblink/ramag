//! SSH + SFTP 工具：连接配置、远程文件、交互终端与传输队列。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod actions;
mod views;

use std::sync::Arc;

use gpui::{App, AppContext as _, Entity, Window};
use ramag_app::SshService;
use ramag_domain::traits::{Tool, ToolMeta};

pub use actions::{CloseSshTerminal, NewSshTerminal, RefreshSftp};
pub use views::SshView;

pub fn create_ssh_view(
    service: Arc<SshService>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SshView> {
    cx.new(|cx| SshView::new(service, window, cx))
}

pub struct SshTool {
    meta: ToolMeta,
}

impl SshTool {
    pub const ID: &'static str = "ssh";

    pub fn new() -> Self {
        Self {
            meta: ToolMeta::new(Self::ID, "SSH + SFTP", "远程终端、文件浏览与安全传输")
                .with_icon("terminal"),
        }
    }
}

impl Default for SshTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for SshTool {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata_exposes_ssh_entry() {
        let tool = SshTool::new();
        assert_eq!(tool.meta().id, "ssh");
        assert!(tool.meta().icon.is_some());
    }
}
