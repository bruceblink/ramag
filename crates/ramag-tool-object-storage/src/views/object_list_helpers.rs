use std::sync::Arc;

use gpui::SharedString;
use ramag_domain::entities::{
    ObjectEntry, ObjectEntryKind, contains_case_insensitive, format_bytes,
};

pub(super) const OBJECT_ROW_HEIGHT: f32 = 28.0;

pub(super) fn sort_object_entries(entries: &mut [ObjectEntry]) {
    entries.sort_unstable_by(|left, right| {
        object_kind_rank(left.kind)
            .cmp(&object_kind_rank(right.kind))
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.key.cmp(&right.key))
    });
}

fn object_kind_rank(kind: ObjectEntryKind) -> u8 {
    match kind {
        ObjectEntryKind::Prefix => 0,
        ObjectEntryKind::Object => 1,
    }
}

pub(super) fn filtered_object_entry_indices(
    entries: &[ObjectEntry],
    query: &str,
) -> Option<Arc<Vec<usize>>> {
    (!query.is_empty()).then(|| {
        Arc::new(
            entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    contains_case_insensitive(&entry.display_name, query).then_some(index)
                })
                .collect(),
        )
    })
}

pub(super) fn object_counts_at(entries: &[ObjectEntry], indices: &[usize]) -> (usize, usize) {
    indices.iter().filter_map(|index| entries.get(*index)).fold(
        (0, 0),
        |(directories, objects), entry| match entry.kind {
            ObjectEntryKind::Prefix => (directories + 1, objects),
            ObjectEntryKind::Object => (directories, objects + 1),
        },
    )
}

pub(super) fn object_breadcrumbs(prefix: &str) -> Vec<(SharedString, String)> {
    let mut parts = vec![(SharedString::from("/"), String::new())];
    let mut target = String::new();
    for component in prefix.trim_end_matches('/').split('/') {
        if component.is_empty() {
            continue;
        }
        target.push_str(component);
        target.push('/');
        parts.push((SharedString::from(component.to_string()), target.clone()));
    }
    parts
}

pub(super) fn object_modified_label(kind: ObjectEntryKind, modified: Option<String>) -> String {
    if kind == ObjectEntryKind::Prefix {
        "—".into()
    } else {
        modified.unwrap_or_else(|| "—".into())
    }
}

pub(super) fn object_type_label(kind: ObjectEntryKind) -> &'static str {
    match kind {
        ObjectEntryKind::Prefix => "文件夹",
        ObjectEntryKind::Object => "文件",
    }
}

pub(super) fn object_size_label(
    kind: ObjectEntryKind,
    size: Option<u64>,
    operable: bool,
) -> String {
    if !operable {
        "仅查看".into()
    } else if kind == ObjectEntryKind::Prefix {
        "—".into()
    } else {
        size.map(format_bytes).unwrap_or_default()
    }
}
