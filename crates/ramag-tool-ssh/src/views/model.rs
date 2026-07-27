//! SSH 工具纯 UI 状态。

use std::sync::Arc;

use gpui::{Entity, SharedString};
use ramag_domain::entities::{RemoteEntry, SshProfile, SshProfileId};
use ramag_terminal::TerminalView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewMode {
    Manager,
    Workspace,
}

pub(super) struct Notice {
    pub message: String,
    pub error: bool,
}

impl Notice {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: true,
        }
    }
}

pub(super) struct TerminalTab {
    pub id: u64,
    pub label: SharedString,
    pub view: Entity<TerminalView>,
}

pub(super) struct SshWorkspace {
    pub profile: SshProfile,
    pub path: String,
    pub entries: Arc<Vec<RemoteEntry>>,
    pub selected_path: Option<String>,
    pub terminals: Vec<TerminalTab>,
    pub active_terminal_id: Option<u64>,
    pub terminal_loading: bool,
    pub sftp_loading: bool,
    pub sftp_error: Option<String>,
    pub operation_busy: bool,
    pub directory_generation: u64,
    pub terminal_generation: u64,
}

impl SshWorkspace {
    pub fn placeholder(profile: SshProfile, path: String) -> Self {
        Self {
            profile,
            path,
            entries: Arc::new(Vec::new()),
            selected_path: None,
            terminals: Vec::new(),
            active_terminal_id: None,
            terminal_loading: false,
            sftp_loading: false,
            sftp_error: None,
            operation_busy: false,
            directory_generation: 0,
            terminal_generation: 0,
        }
    }

    pub fn profile_id(&self) -> &SshProfileId {
        &self.profile.id
    }
}
