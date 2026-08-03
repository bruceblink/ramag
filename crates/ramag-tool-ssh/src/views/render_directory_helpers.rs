//! SSH 远程目录行、筛选、计数与路径拆分。

use std::cmp::Ordering;
use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, MouseButton, ParentElement, Pixels, Point,
    Render, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    spinner::Spinner,
    v_flex,
};
use ramag_domain::entities::{
    RemoteEntry, RemoteEntryKind, SshProfileId, contains_case_insensitive, validate_remote_path,
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

#[derive(Clone)]
pub(super) struct RemoteDirectoryDrag {
    pub workspace_id: SshProfileId,
    pub path: String,
    name: String,
    position: Point<Pixels>,
}

impl RemoteDirectoryDrag {
    fn from_entry(workspace_id: SshProfileId, entry: &RemoteEntry) -> Option<Self> {
        if entry.kind != RemoteEntryKind::Directory {
            return None;
        }
        Self::new(workspace_id, entry.path.clone(), entry.name.clone())
    }

    pub(super) fn from_current_path(workspace_id: SshProfileId, path: &str) -> Option<Self> {
        Self::new(workspace_id, path.to_string(), path.to_string())
    }

    fn new(workspace_id: SshProfileId, path: String, name: String) -> Option<Self> {
        validate_remote_path(&path).ok()?;
        if !path.starts_with('/') {
            return None;
        }
        Some(Self {
            workspace_id,
            path,
            name,
            position: Point::default(),
        })
    }

    pub(super) fn position(mut self, position: Point<Pixels>) -> Self {
        self.position = position;
        self
    }
}

impl Render for RemoteDirectoryDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(self.position.x + px(12.0))
            .pt(self.position.y + px(12.0))
            .child(
                h_flex()
                    .max_w(px(280.0))
                    .items_center()
                    .gap(px(7.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_md()
                    .child(
                        Icon::new(IconName::Folder)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.name.clone()),
                    ),
            )
    }
}

pub(super) fn sort_remote_entries(entries: &mut [RemoteEntry]) {
    entries.sort_unstable_by(|left, right| {
        remote_entry_kind_rank(left.kind)
            .cmp(&remote_entry_kind_rank(right.kind))
            .then_with(|| natural_ascii_cmp(&left.name, &right.name))
    });
}

fn remote_entry_kind_rank(kind: RemoteEntryKind) -> u8 {
    match kind {
        RemoteEntryKind::Directory => 0,
        RemoteEntryKind::File => 1,
        RemoteEntryKind::Symlink => 2,
        RemoteEntryKind::Other => 3,
    }
}

/// 常见 ASCII 文件名按数字大小排序；非 ASCII 仍保持稳定的 UTF-8 字节序。
fn natural_ascii_cmp(left: &str, right: &str) -> Ordering {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let (mut left_index, mut right_index) = (0usize, 0usize);

    while left_index < left_bytes.len() && right_index < right_bytes.len() {
        let left_byte = left_bytes[left_index];
        let right_byte = right_bytes[right_index];
        if left_byte.is_ascii_digit() && right_byte.is_ascii_digit() {
            let left_end = digit_run_end(left_bytes, left_index);
            let right_end = digit_run_end(right_bytes, right_index);
            let left_significant = significant_digit_start(left_bytes, left_index, left_end);
            let right_significant = significant_digit_start(right_bytes, right_index, right_end);
            let ordering = (left_end - left_significant)
                .cmp(&(right_end - right_significant))
                .then_with(|| {
                    left_bytes[left_significant..left_end]
                        .cmp(&right_bytes[right_significant..right_end])
                })
                .then_with(|| (left_end - left_index).cmp(&(right_end - right_index)));
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index = left_end;
            right_index = right_end;
            continue;
        }

        let ordering = left_byte
            .to_ascii_lowercase()
            .cmp(&right_byte.to_ascii_lowercase());
        if ordering != Ordering::Equal {
            return ordering;
        }
        left_index += 1;
        right_index += 1;
    }

    left_bytes
        .len()
        .cmp(&right_bytes.len())
        .then_with(|| left.cmp(right))
}

fn digit_run_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn significant_digit_start(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut significant = start;
    while significant + 1 < end && bytes[significant] == b'0' {
        significant += 1;
    }
    significant
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
    let entry_drag = menu_state
        .connection_available
        .then(|| RemoteDirectoryDrag::from_entry(workspace_id.clone(), &entry))
        .flatten();
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
        )
        .when_some(entry_drag, |row, drag| {
            row.cursor_pointer().on_drag(drag, |drag, position, _, cx| {
                cx.new(|_| drag.clone().position(position))
            })
        });
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
    fn terminal_drag_accepts_only_absolute_directories() {
        let entry = |path: &str, kind| RemoteEntry {
            name: path.rsplit('/').next().unwrap_or_default().into(),
            path: path.into(),
            kind,
            size: 0,
            permissions: None,
            modified_at: None,
        };

        let profile_id = SshProfileId::new();
        let directory = entry("/srv/app", RemoteEntryKind::Directory);

        assert_eq!(
            RemoteDirectoryDrag::from_entry(profile_id.clone(), &directory).map(|drag| drag.path),
            Some("/srv/app".into())
        );
        assert!(
            RemoteDirectoryDrag::from_entry(
                profile_id.clone(),
                &entry("/srv/app/main.rs", RemoteEntryKind::File)
            )
            .is_none()
        );
        assert!(
            RemoteDirectoryDrag::from_current_path(profile_id.clone(), "relative/path").is_none()
        );
        assert!(RemoteDirectoryDrag::from_current_path(profile_id, "/tmp/line\nbreak").is_none());
    }

    #[test]
    fn remote_entries_sort_directories_first_and_names_naturally() {
        let entry = |name: &str, kind| RemoteEntry {
            name: name.into(),
            path: format!("/{name}"),
            kind,
            size: 0,
            permissions: None,
            modified_at: None,
        };
        let mut entries = vec![
            entry("file10", RemoteEntryKind::File),
            entry("Zoo", RemoteEntryKind::Directory),
            entry("file2", RemoteEntryKind::File),
            entry("alpha", RemoteEntryKind::Directory),
            entry("file02", RemoteEntryKind::File),
        ];

        sort_remote_entries(&mut entries);

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "Zoo", "file2", "file02", "file10"]
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
