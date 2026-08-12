use super::*;
use crate::views::object_list_helpers::sort_object_entries;
use ramag_domain::entities::ObjectEntry;

fn entry(name: &str, kind: ObjectEntryKind) -> ObjectEntry {
    ObjectEntry {
        key: if kind == ObjectEntryKind::Prefix {
            format!("{name}/")
        } else {
            name.into()
        },
        display_name: name.into(),
        kind,
        operable: true,
        size: None,
        last_modified: None,
        etag: None,
        content_type: None,
        storage_class: None,
    }
}

#[test]
fn object_rows_match_shared_compact_density() {
    assert_eq!(OBJECT_ROW_HEIGHT, 28.0);
}

#[test]
fn directories_display_their_type_instead_of_a_fake_size() {
    assert_eq!(object_type_label(ObjectEntryKind::Prefix), "文件夹");
    assert_eq!(
        object_size_label(ObjectEntryKind::Prefix, Some(0), true),
        "—"
    );
    assert_eq!(
        object_size_label(ObjectEntryKind::Object, Some(0), true),
        "0 B"
    );
}

#[test]
fn unsafe_keys_use_status_instead_of_size() {
    assert_eq!(
        object_size_label(ObjectEntryKind::Object, Some(1024), false),
        "仅查看"
    );
}

#[test]
fn object_entries_sort_directories_first_then_by_name() {
    let mut entries = vec![
        entry("z.txt", ObjectEntryKind::Object),
        entry("beta", ObjectEntryKind::Prefix),
        entry("a.txt", ObjectEntryKind::Object),
        entry("alpha", ObjectEntryKind::Prefix),
    ];

    sort_object_entries(&mut entries);

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.display_name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "a.txt", "z.txt"]
    );
}

#[test]
fn virtual_directory_time_is_explicitly_unavailable() {
    assert_eq!(object_modified_label(ObjectEntryKind::Prefix, None), "—");
    assert_eq!(object_modified_label(ObjectEntryKind::Object, None), "—");
}

#[test]
fn current_directory_filter_is_case_insensitive_contains_search() {
    let entries = vec![
        entry("Report.JSON", ObjectEntryKind::Object),
        entry("logs", ObjectEntryKind::Prefix),
    ];

    let indices = filtered_object_entry_indices(&entries, "port")
        .expect("a non-empty query returns filtered indices");

    assert_eq!(indices.as_slice(), [0]);
}

#[test]
fn object_prefix_builds_clickable_breadcrumb_targets() {
    let parts = object_breadcrumbs("gewu/structure/model/");
    assert_eq!(
        parts
            .iter()
            .map(|(label, target)| (label.as_ref(), target.as_str()))
            .collect::<Vec<_>>(),
        [
            ("/", ""),
            ("gewu", "gewu/"),
            ("structure", "gewu/structure/"),
            ("model", "gewu/structure/model/"),
        ]
    );
}
