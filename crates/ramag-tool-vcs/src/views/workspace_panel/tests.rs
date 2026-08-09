use super::*;

#[test]
fn workspace_rows_cache_requires_all_inputs_to_match() {
    let rows = Rc::new(Vec::new());
    let key = WorkspaceRowsCacheKey {
        status_request_seq: 11,
        files_identity: 17,
        files_len: 3,
        collapsed_version: 2,
        query: "src".into(),
    };
    let cache = WorkspaceRowsCacheEntry {
        key: key.clone(),
        rows: rows.clone(),
    };

    let cached = cache.get(&key);
    assert!(cached.is_some());
    if let Some(cached) = cached {
        assert!(Rc::ptr_eq(&cached, &rows));
    }

    let mut changed = key.clone();
    changed.status_request_seq += 1;
    assert!(cache.get(&changed).is_none());

    let mut changed = key.clone();
    changed.query = "tests".into();
    assert!(cache.get(&changed).is_none());

    changed = key;
    changed.collapsed_version += 1;
    assert!(cache.get(&changed).is_none());
}

#[test]
fn parent_directory_set_excludes_file_names() {
    let dirs = collect_parent_dirs(["src/ui/view.rs", "README.md"]);

    assert_eq!(
        dirs,
        HashSet::from(["src".to_string(), "src/ui".to_string()])
    );
}
