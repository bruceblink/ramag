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
    pub directory_query: String,
    pub entries: Arc<Vec<RemoteEntry>>,
    pub selected_path: Option<String>,
    pub terminals: Vec<TerminalTab>,
    pub active_terminal_id: Option<u64>,
    pub terminal_loading: bool,
    pub connection_started: bool,
    pub sftp_loading: bool,
    pub directory_loading_path: Option<String>,
    pub sftp_error: Option<String>,
    pub operation_busy: bool,
    pub file_preview_loading: bool,
    pub transfers_visible: bool,
    pub next_terminal_ordinal: u64,
    pub directory_generation: u64,
    pub file_preview_generation: u64,
    pub terminal_generation: u64,
}

impl SshWorkspace {
    pub fn placeholder(profile: SshProfile, path: String) -> Self {
        Self {
            profile,
            path,
            directory_query: String::new(),
            entries: Arc::new(Vec::new()),
            selected_path: None,
            terminals: Vec::new(),
            active_terminal_id: None,
            terminal_loading: false,
            connection_started: false,
            sftp_loading: false,
            directory_loading_path: None,
            sftp_error: None,
            operation_busy: false,
            file_preview_loading: false,
            transfers_visible: false,
            next_terminal_ordinal: 1,
            directory_generation: 0,
            file_preview_generation: 0,
            terminal_generation: 0,
        }
    }

    pub fn profile_id(&self) -> &SshProfileId {
        &self.profile.id
    }

    pub fn next_terminal_label(&mut self) -> SharedString {
        let ordinal = self.next_terminal_ordinal;
        self.next_terminal_ordinal = self.next_terminal_ordinal.wrapping_add(1).max(1);
        format!("终端 {ordinal}").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_labels_remain_unique_when_tabs_are_removed() {
        let mut workspace =
            SshWorkspace::placeholder(SshProfile::new("server", "host"), "/".into());

        assert_eq!(workspace.next_terminal_label().as_ref(), "终端 1");
        assert_eq!(workspace.next_terminal_label().as_ref(), "终端 2");
    }
}
