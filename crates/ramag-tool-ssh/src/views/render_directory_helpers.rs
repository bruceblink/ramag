//! SSH 远程目录行、筛选、计数与路径拆分。

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, MouseButton, ParentElement, SharedString, Styled,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    spinner::Spinner,
    v_flex,
};
use ramag_domain::entities::{
    RemoteEntry, RemoteEntryKind, SshProfileId, contains_case_insensitive,
};

use super::SshView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteEntryAction {
    Open,
    Preview,
    Download,
    Rename,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteEntryActivation {
    OpenDirectory,
    PreviewFile,
    Unsupported,
}

#[derive(Clone, Copy)]
pub(super) struct RemoteEntryMenuState {
    pub connection_available: bool,
    pub allow_write: bool,
    pub directory_loading: bool,
    pub operation_busy: bool,
    pub preview_loading: bool,
}

pub(super) fn remote_entry_row(
    entry: RemoteEntry,
    selected_path: Option<&String>,
    workspace_id: SshProfileId,
    index: usize,
    loading: bool,
    menu_state: RemoteEntryMenuState,
    cx: &mut Context<SshView>,
) -> AnyElement {
    let selected = selected_path == Some(&entry.path);
    let icon = match entry.kind {
        RemoteEntryKind::Directory => IconName::Folder,
        RemoteEntryKind::Symlink => IconName::ExternalLink,
        RemoteEntryKind::File | RemoteEntryKind::Other => IconName::File,
    };
    let entry_for_click = entry.clone();
    let workspace_for_right_click = workspace_id.clone();
    let path_for_right_click = entry.path.clone();
    let actions = remote_entry_actions(entry.kind, menu_state.allow_write);
    let has_context_menu = !actions.is_empty();
    let entity_for_menu = cx.entity();
    let workspace_for_menu = workspace_id.clone();
    let entry_for_menu = entry.clone();
    let selector = format!("sftp-entry-{index}");
    let row = h_flex()
        .id(("sftp-entry", index))
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(28.0))
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(if selected {
            cx.theme().muted
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().muted))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.select_remote_entry(workspace_id.clone(), entry_for_click.path.clone(), cx);
            if event.click_count() >= 2 && !loading {
                this.activate_remote_entry(
                    workspace_id.clone(),
                    entry_for_click.clone(),
                    window,
                    cx,
                );
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _, _, cx| {
                this.select_remote_entry(
                    workspace_for_right_click.clone(),
                    path_for_right_click.clone(),
                    cx,
                );
            }),
        )
        .child(if loading {
            div()
                .id(("sftp-entry-loading", index))
                .debug_selector(move || format!("sftp-entry-loading-{index}"))
                .child(Spinner::new().xsmall())
                .into_any_element()
        } else {
            Icon::new(icon)
                .small()
                .text_color(cx.theme().muted_foreground)
                .into_any_element()
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .text_ellipsis()
                .child(entry.name),
        );
    if has_context_menu {
        row.context_menu(move |menu, _, _| {
            remote_entry_context_menu(
                menu,
                entity_for_menu.clone(),
                workspace_for_menu.clone(),
                entry_for_menu.clone(),
                menu_state,
            )
        })
        .into_any_element()
    } else {
        row.into_any_element()
    }
}

pub(super) fn remote_entry_activation(kind: RemoteEntryKind) -> RemoteEntryActivation {
    match kind {
        RemoteEntryKind::Directory => RemoteEntryActivation::OpenDirectory,
        RemoteEntryKind::File => RemoteEntryActivation::PreviewFile,
        RemoteEntryKind::Symlink | RemoteEntryKind::Other => RemoteEntryActivation::Unsupported,
    }
}

fn remote_entry_actions(kind: RemoteEntryKind, allow_write: bool) -> Vec<RemoteEntryAction> {
    let mut actions = match kind {
        RemoteEntryKind::Directory => {
            vec![RemoteEntryAction::Open, RemoteEntryAction::Download]
        }
        RemoteEntryKind::File => vec![RemoteEntryAction::Preview, RemoteEntryAction::Download],
        RemoteEntryKind::Symlink | RemoteEntryKind::Other => Vec::new(),
    };
    if allow_write {
        actions.extend([RemoteEntryAction::Rename, RemoteEntryAction::Delete]);
    }
    actions
}

fn remote_entry_context_menu(
    mut menu: PopupMenu,
    entity: gpui::Entity<SshView>,
    workspace_id: SshProfileId,
    entry: RemoteEntry,
    state: RemoteEntryMenuState,
) -> PopupMenu {
    let actions = remote_entry_actions(entry.kind, state.allow_write);
    let mut write_separator_added = false;
    for (index, action) in actions.into_iter().enumerate() {
        if index > 0 && action.is_write() && !write_separator_added {
            menu = menu.separator();
            write_separator_added = true;
        }
        let disabled = action.is_disabled(state);
        let action_entity = entity.clone();
        let action_workspace = workspace_id.clone();
        let action_entry = entry.clone();
        menu = menu.item(
            ramag_ui::menu_item_with_disabled(action.label(), disabled).on_click(
                move |_, window, app| {
                    action_entity.update(app, |this, cx| match action {
                        RemoteEntryAction::Open => this.refresh_directory(
                            action_workspace.clone(),
                            Some(action_entry.path.clone()),
                            cx,
                        ),
                        RemoteEntryAction::Preview => this.preview_remote_file(
                            action_workspace.clone(),
                            action_entry.clone(),
                            window,
                            cx,
                        ),
                        RemoteEntryAction::Download => this.pick_download(
                            action_workspace.clone(),
                            action_entry.clone(),
                            window,
                            cx,
                        ),
                        RemoteEntryAction::Rename => this.prompt_rename_entry(
                            action_workspace.clone(),
                            action_entry.clone(),
                            window,
                            cx,
                        ),
                        RemoteEntryAction::Delete => this.request_delete_entry(
                            action_workspace.clone(),
                            action_entry.clone(),
                            window,
                            cx,
                        ),
                    });
                },
            ),
        );
    }
    menu
}

impl RemoteEntryAction {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "打开",
            Self::Preview => "查看",
            Self::Download => "下载",
            Self::Rename => "改名",
            Self::Delete => "删除",
        }
    }

    fn is_write(self) -> bool {
        matches!(self, Self::Rename | Self::Delete)
    }

    fn is_disabled(self, state: RemoteEntryMenuState) -> bool {
        if !state.connection_available {
            return true;
        }
        match self {
            Self::Open => state.directory_loading,
            Self::Preview => state.preview_loading,
            Self::Download => false,
            Self::Rename | Self::Delete => state.operation_busy,
        }
    }
}

pub(super) fn directory_counts(entries: &[RemoteEntry]) -> (usize, usize) {
    entries
        .iter()
        .fold((0, 0), |(directories, files), entry| match entry.kind {
            RemoteEntryKind::Directory => (directories + 1, files),
            RemoteEntryKind::File => (directories, files + 1),
            RemoteEntryKind::Symlink | RemoteEntryKind::Other => (directories, files),
        })
}

pub(super) fn directory_counts_at(entries: &[RemoteEntry], indices: &[usize]) -> (usize, usize) {
    indices.iter().filter_map(|index| entries.get(*index)).fold(
        (0, 0),
        |(directories, files), entry| match entry.kind {
            RemoteEntryKind::Directory => (directories + 1, files),
            RemoteEntryKind::File => (directories, files + 1),
            RemoteEntryKind::Symlink | RemoteEntryKind::Other => (directories, files),
        },
    )
}

pub(super) fn filtered_entry_indices(
    entries: &[RemoteEntry],
    query: &str,
) -> Option<Arc<Vec<usize>>> {
    (!query.is_empty()).then(|| {
        Arc::new(
            entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    contains_case_insensitive(&entry.name, query).then_some(index)
                })
                .collect(),
        )
    })
}

pub(super) fn remote_breadcrumbs(path: &str) -> Vec<(SharedString, String)> {
    if !path.starts_with('/') {
        return vec![(SharedString::from(path.to_string()), path.to_string())];
    }
    let mut parts = vec![(SharedString::from("/"), "/".to_string())];
    let mut target = String::new();
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        target.push('/');
        target.push_str(segment);
        parts.push((SharedString::from(segment.to_string()), target.clone()));
    }
    parts
}

pub(super) fn centered_message(message: &'static str, cx: &gpui::App) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_counts_only_files_and_directories() {
        let entry = |name: &str, kind| RemoteEntry {
            name: name.into(),
            path: format!("/{name}"),
            kind,
            size: 0,
            permissions: None,
            modified_at: None,
        };
        let entries = [
            entry("src", RemoteEntryKind::Directory),
            entry("README.md", RemoteEntryKind::File),
            entry("latest", RemoteEntryKind::Symlink),
        ];

        assert_eq!(directory_counts(&entries), (1, 1));
        assert_eq!(
            filtered_entry_indices(&entries, "read")
                .expect("query should create an index")
                .as_slice(),
            &[1]
        );
    }

    #[test]
    fn absolute_remote_path_builds_clickable_ancestors() {
        let targets = remote_breadcrumbs("/home/alice/project")
            .into_iter()
            .map(|(_, target)| target)
            .collect::<Vec<_>>();

        assert_eq!(
            targets,
            ["/", "/home", "/home/alice", "/home/alice/project"]
        );
    }

    #[test]
    fn remote_entry_actions_match_kind_and_write_permission() {
        assert_eq!(
            remote_entry_actions(RemoteEntryKind::Directory, true),
            [
                RemoteEntryAction::Open,
                RemoteEntryAction::Download,
                RemoteEntryAction::Rename,
                RemoteEntryAction::Delete,
            ]
        );
        assert_eq!(
            remote_entry_actions(RemoteEntryKind::File, true),
            [
                RemoteEntryAction::Preview,
                RemoteEntryAction::Download,
                RemoteEntryAction::Rename,
                RemoteEntryAction::Delete,
            ]
        );
        assert_eq!(
            remote_entry_actions(RemoteEntryKind::File, false),
            [RemoteEntryAction::Preview, RemoteEntryAction::Download]
        );
        assert_eq!(
            remote_entry_actions(RemoteEntryKind::Directory, false),
            [RemoteEntryAction::Open, RemoteEntryAction::Download]
        );
        assert_eq!(
            remote_entry_actions(RemoteEntryKind::Symlink, true),
            [RemoteEntryAction::Rename, RemoteEntryAction::Delete]
        );
        assert!(remote_entry_actions(RemoteEntryKind::Other, false).is_empty());
    }

    #[test]
    fn double_click_routes_files_to_preview() {
        assert_eq!(
            remote_entry_activation(RemoteEntryKind::Directory),
            RemoteEntryActivation::OpenDirectory
        );
        assert_eq!(
            remote_entry_activation(RemoteEntryKind::File),
            RemoteEntryActivation::PreviewFile
        );
        assert_eq!(
            remote_entry_activation(RemoteEntryKind::Symlink),
            RemoteEntryActivation::Unsupported
        );
        assert_eq!(
            remote_entry_activation(RemoteEntryKind::Other),
            RemoteEntryActivation::Unsupported
        );
    }

    #[test]
    fn remote_entry_action_labels_are_two_characters() {
        for action in [
            RemoteEntryAction::Open,
            RemoteEntryAction::Preview,
            RemoteEntryAction::Download,
            RemoteEntryAction::Rename,
            RemoteEntryAction::Delete,
        ] {
            assert_eq!(action.label().chars().count(), 2);
        }
    }
}
