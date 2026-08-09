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
    assert!(RemoteDirectoryDrag::from_current_path(profile_id.clone(), "relative/path").is_none());
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

    let windows = remote_breadcrumbs("C:/Users/Admin")
        .into_iter()
        .map(|(_, target)| target)
        .collect::<Vec<_>>();
    assert_eq!(windows, ["C:/", "C:/Users", "C:/Users/Admin"]);

    let virtual_windows = remote_breadcrumbs("/C:/Users/Admin")
        .into_iter()
        .map(|(label, target)| (label.to_string(), target))
        .collect::<Vec<_>>();
    assert_eq!(
        virtual_windows,
        [
            ("/".into(), "/".into()),
            ("C:".into(), "/C:/".into()),
            ("Users".into(), "/C:/Users".into()),
            ("Admin".into(), "/C:/Users/Admin".into()),
        ]
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
    assert!(remote_entry_actions(RemoteEntryKind::Symlink, true).is_empty());
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
