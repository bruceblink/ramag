use std::sync::Arc;

use gpui::{Entity, SharedString};
use ramag_domain::entities::{RemoteEntry, SshProfile, SshProfileId, SshRemoteCapabilities};
use ramag_terminal::TerminalView;

pub(super) fn can_close_terminal(terminal_count: usize) -> bool {
    terminal_count > 1
}

pub(super) fn terminal_index_after_close(
    closed_index: usize,
    remaining_count: usize,
) -> Option<usize> {
    (remaining_count > 0).then(|| closed_index.saturating_sub(1).min(remaining_count - 1))
}

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
    pub directory_loaded: bool,
    pub directory_loading_path: Option<String>,
    pub sftp_error: Option<String>,
    pub operation_busy: bool,
    pub file_preview_loading: bool,
    pub transfers_visible: bool,
    pub next_terminal_ordinal: u64,
    pub directory_generation: u64,
    pub file_preview_generation: u64,
    pub terminal_generation: u64,
    pub capabilities: Option<SshRemoteCapabilities>,
    pub capability_error: Option<String>,
    pub capability_loading: bool,
    pub capability_generation: u64,
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
            directory_loaded: false,
            directory_loading_path: None,
            sftp_error: None,
            operation_busy: false,
            file_preview_loading: false,
            transfers_visible: false,
            next_terminal_ordinal: 1,
            directory_generation: 0,
            file_preview_generation: 0,
            terminal_generation: 0,
            capabilities: None,
            capability_error: None,
            capability_loading: false,
            capability_generation: 0,
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
    fn terminal_close_keeps_at_least_one_tab() {
        assert!(!can_close_terminal(0));
        assert!(!can_close_terminal(1));
        assert!(can_close_terminal(2));
    }

    #[test]
    fn closing_active_terminal_selects_the_previous_tab() {
        assert_eq!(terminal_index_after_close(9, 9), Some(8));
        assert_eq!(terminal_index_after_close(5, 9), Some(4));
        assert_eq!(terminal_index_after_close(0, 9), Some(0));
        assert_eq!(terminal_index_after_close(0, 0), None);
    }

    #[test]
    fn terminal_labels_remain_unique_when_tabs_are_removed() {
        let mut workspace =
            SshWorkspace::placeholder(SshProfile::new("server", "host"), "/".into());

        assert_eq!(workspace.next_terminal_label().as_ref(), "终端 1");
        assert_eq!(workspace.next_terminal_label().as_ref(), "终端 2");
    }
}
