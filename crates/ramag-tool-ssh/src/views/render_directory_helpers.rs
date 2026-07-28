//! SSH 远程目录行、筛选、计数与路径拆分。

use std::sync::Arc;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};
use ramag_domain::entities::{
    RemoteEntry, RemoteEntryKind, SshProfileId, contains_case_insensitive,
};

use super::SshView;

pub(super) fn remote_entry_row(
    entry: RemoteEntry,
    selected_path: Option<&String>,
    workspace_id: SshProfileId,
    index: usize,
    cx: &mut Context<SshView>,
) -> AnyElement {
    let selected = selected_path == Some(&entry.path);
    let icon = match entry.kind {
        RemoteEntryKind::Directory => IconName::Folder,
        RemoteEntryKind::Symlink => IconName::ExternalLink,
        RemoteEntryKind::File | RemoteEntryKind::Other => IconName::File,
    };
    let entry_for_click = entry.clone();
    let selector = format!("sftp-entry-{index}");
    h_flex()
        .id(("sftp-entry", index))
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(36.0))
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
            if event.click_count() >= 2 {
                this.activate_remote_entry(
                    workspace_id.clone(),
                    entry_for_click.clone(),
                    window,
                    cx,
                );
            }
        }))
        .child(
            Icon::new(icon)
                .small()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .overflow_hidden()
                .text_ellipsis()
                .child(entry.name),
        )
        .into_any_element()
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
}
