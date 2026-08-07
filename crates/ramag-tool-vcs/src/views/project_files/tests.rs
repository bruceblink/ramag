use super::*;

#[test]
#[ignore = "手动观察十万文件折叠树构建耗时"]
fn reports_large_collapsed_project_tree_latency() {
    use std::hint::black_box;
    use std::time::Instant;

    const FILES: usize = 100_000;
    const ITERATIONS: usize = 5;
    let paths = (0..FILES)
        .map(|index| format!("files/file{index:06}.rs"))
        .collect::<Vec<_>>();
    let mut samples = Vec::with_capacity(ITERATIONS);
    let collapsed = std::collections::HashSet::new();
    let indices = (0..paths.len()).collect::<Vec<_>>();
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        black_box(build_project_rows(&paths, &indices, &collapsed));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let expanded = std::collections::HashSet::from(["files".to_string()]);
    let started = Instant::now();
    let expanded_rows = black_box(build_project_rows(&paths, &indices, &expanded));
    let expanded_elapsed = started.elapsed();
    eprintln!(
        "vcs project tree: files={FILES}, collapsed_median={:.3} ms, expanded={:.3} ms, expanded_rows={}",
        samples[ITERATIONS / 2].as_secs_f64() * 1_000.0,
        expanded_elapsed.as_secs_f64() * 1_000.0,
        expanded_rows.len()
    );
}

fn status(
    path: &str,
    staged: Option<FileChangeKind>,
    unstaged: Option<FileChangeKind>,
) -> FileStatus {
    FileStatus {
        path: path.into(),
        old_path: None,
        staged,
        unstaged,
    }
}

#[test]
fn collapsed_tree_does_not_materialize_hidden_descendants() {
    let paths = ["README.md", "src/a.rs", "src/nested/b.rs"]
        .map(str::to_string)
        .to_vec();
    let rows = build_project_rows(&paths, &[0, 1, 2], &std::collections::HashSet::new());

    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0], ProjectRow::Dir { name, .. } if name == "src"));
    assert!(
        matches!(&rows[1], ProjectRow::File { name, path_index: 0, .. } if name == "README.md")
    );
}

#[test]
fn expanded_tree_materializes_only_open_levels() {
    let paths = ["src/a.rs", "src/nested/b.rs"].map(str::to_string).to_vec();
    let expanded = std::collections::HashSet::from(["src".to_string()]);
    let rows = build_project_rows(&paths, &[0, 1], &expanded);

    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[0], ProjectRow::Dir { name, is_expanded: true, .. } if name == "src"));
    assert!(
        matches!(&rows[1], ProjectRow::Dir { name, is_expanded: false, depth: 1, .. } if name == "nested")
    );
    assert!(
        matches!(&rows[2], ProjectRow::File { name, path_index: 0, depth: 1 } if name == "a.rs")
    );
}

#[test]
fn status_kind_map_keeps_display_precedence() {
    let project_files = vec![
        "clean.rs".to_string(),
        "conflict.rs".to_string(),
        "modified.rs".to_string(),
    ];
    let files = vec![
        status(
            "modified.rs",
            Some(FileChangeKind::Added),
            Some(FileChangeKind::Modified),
        ),
        status(
            "conflict.rs",
            Some(FileChangeKind::Conflicted),
            Some(FileChangeKind::Modified),
        ),
        status("clean.rs", None, None),
    ];

    let kinds = build_status_kind_map(&project_files, &files);

    assert_eq!(kinds.get(&2), Some(&FileChangeKind::Modified));
    assert_eq!(kinds.get(&1), Some(&FileChangeKind::Conflicted));
    assert!(!kinds.contains_key(&0));
}

#[test]
fn status_cache_requires_matching_refresh_identity_and_length() {
    let kinds = Rc::new(HashMap::new());
    let cache = ProjectStatusCacheEntry {
        project_files_version: 7,
        status_request_seq: 9,
        files_identity: 11,
        files_len: 2,
        kinds: kinds.clone(),
    };

    let cached = cache.get(7, 9, 11, 2);
    assert!(cached.is_some());
    if let Some(cached) = cached {
        assert!(Rc::ptr_eq(&cached, &kinds));
    }
    assert!(cache.get(8, 9, 11, 2).is_none());
    assert!(cache.get(7, 10, 11, 2).is_none());
    assert!(cache.get(7, 9, 12, 2).is_none());
    assert!(cache.get(7, 9, 11, 3).is_none());
}

#[test]
fn ancestors_are_collected_incrementally() {
    let ancestors = collect_ancestors(&["a/b/c/file.rs".to_string()]);

    assert_eq!(ancestors.len(), 3);
    assert!(ancestors.contains("a"));
    assert!(ancestors.contains("a/b"));
    assert!(ancestors.contains("a/b/c"));
}
