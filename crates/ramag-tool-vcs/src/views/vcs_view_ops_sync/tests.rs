use super::*;

#[test]
#[ignore = "手动观察十万文件增量成员合并耗时"]
fn reports_large_project_patch_merge_latency() {
    use std::hint::black_box;
    use std::time::Instant;

    const FILES: usize = 100_000;
    const ITERATIONS: usize = 1_000;
    let files = (0..FILES)
        .map(|index| format!("files/file{index:06}.rs"))
        .collect::<Vec<_>>();
    let mut current = files;
    let path = "files/file050000.rs".to_string();
    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        black_box(merge_partial_project_files(
            &mut current,
            std::slice::from_ref(&path),
            vec![path.clone()],
        ));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    eprintln!(
        "vcs project patch merge: files={FILES}, median={:.3} us",
        samples[ITERATIONS / 2].as_secs_f64() * 1_000_000.0
    );
}

#[test]
#[ignore = "手动观察十万条状态增量合并耗时"]
fn reports_large_status_patch_merge_latency() {
    use std::hint::black_box;
    use std::time::Instant;

    const FILES: usize = 100_000;
    const ITERATIONS: usize = 200;
    let files = (0..FILES)
        .map(|index| FileStatus {
            path: format!("files/file{index:06}.rs"),
            old_path: None,
            staged: None,
            unstaged: Some(FileChangeKind::Modified),
        })
        .collect::<Vec<_>>();
    let incoming = files[FILES / 2].clone();
    let mut status = ramag_domain::entities::WorkingTreeStatus {
        files,
        ..Default::default()
    };
    let path = incoming.path.clone();
    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        black_box(merge_partial_status(
            &mut status,
            std::slice::from_ref(&path),
            vec![incoming.clone()],
        ));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    eprintln!(
        "vcs status patch merge: files={FILES}, median={:.3} us",
        samples[ITERATIONS / 2].as_secs_f64() * 1_000_000.0
    );
}

fn fs(staged: Option<FileChangeKind>, unstaged: Option<FileChangeKind>) -> FileStatus {
    FileStatus {
        path: "a.rs".into(),
        old_path: None,
        staged,
        unstaged,
    }
}

#[test]
fn keeps_valid_group() {
    // 同文件先 add 再改：两组都有效，各自保持原组
    let f = fs(
        Some(FileChangeKind::Modified),
        Some(FileChangeKind::Modified),
    );
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Staged),
        GroupKind::Staged
    );
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Unstaged),
        GroupKind::Unstaged
    );
}

#[test]
fn clean_external_checkout_still_counts_as_head_change() {
    let old = ramag_domain::entities::WorkingTreeStatus {
        head_branch: Some("main".into()),
        head_commit: Some("aaaaaaa".into()),
        ..Default::default()
    };
    let new = ramag_domain::entities::WorkingTreeStatus {
        head_branch: Some("feature".into()),
        head_commit: Some("bbbbbbb".into()),
        ..Default::default()
    };
    assert_eq!(status_changes(&old, &new), (false, true));
}

#[test]
fn rename_identity_change_counts_as_file_change() {
    let old = ramag_domain::entities::WorkingTreeStatus {
        files: vec![FileStatus {
            path: "new.rs".into(),
            old_path: Some("first.rs".into()),
            staged: Some(FileChangeKind::Renamed),
            unstaged: None,
        }],
        ..Default::default()
    };
    let mut new = old.clone();
    new.files[0].old_path = Some("second.rs".into());

    assert_eq!(status_changes(&old, &new), (true, false));
}

#[test]
fn stage_moves_unstaged_tab_to_staged() {
    let f = fs(Some(FileChangeKind::Modified), None);
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Unstaged),
        GroupKind::Staged
    );
}

#[test]
fn unstage_moves_staged_tab_back() {
    let f = fs(None, Some(FileChangeKind::Modified));
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Staged),
        GroupKind::Unstaged
    );
}

#[test]
fn staging_untracked_redirects_to_staged() {
    let f = fs(Some(FileChangeKind::Added), None);
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Untracked),
        GroupKind::Staged
    );
}

#[test]
fn conflict_wins_over_everything() {
    let f = fs(Some(FileChangeKind::Conflicted), None);
    assert_eq!(
        redirect_group_kind(&f, GroupKind::Unstaged),
        GroupKind::Conflict
    );
}

#[test]
fn workspace_refresh_signals_are_coalesced() {
    let (tx, mut rx) = futures::channel::mpsc::channel(0);
    let tx = std::sync::Mutex::new(tx);

    enqueue_workspace_refresh(&tx);
    enqueue_workspace_refresh(&tx);

    assert_eq!(rx.try_recv(), Ok(()));
    assert!(rx.try_recv().is_err());
}

#[test]
fn in_flight_workspace_refresh_keeps_only_one_pending_run() {
    let mut in_flight = false;
    let mut pending = RepoRefresh::default();

    assert!(begin_workspace_refresh(
        &mut in_flight,
        &mut pending,
        RepoRefresh {
            paths: vec!["src/lib.rs".into()],
            ..Default::default()
        },
    ));
    assert!(!begin_workspace_refresh(
        &mut in_flight,
        &mut pending,
        RepoRefresh {
            paths: vec!["src/main.rs".into()],
            ..Default::default()
        },
    ));
    assert!(!begin_workspace_refresh(
        &mut in_flight,
        &mut pending,
        RepoRefresh::full(),
    ));
    assert!(in_flight);
    assert_eq!(pending, RepoRefresh::full());
}

#[test]
fn partial_status_replaces_only_covered_paths() {
    let mut status = ramag_domain::entities::WorkingTreeStatus {
        files: vec![
            FileStatus {
                path: "README.md".into(),
                old_path: None,
                staged: None,
                unstaged: Some(FileChangeKind::Modified),
            },
            FileStatus {
                path: "src/lib.rs".into(),
                old_path: None,
                staged: None,
                unstaged: Some(FileChangeKind::Modified),
            },
        ],
        ..Default::default()
    };
    let incoming = vec![FileStatus {
        path: "src/main.rs".into(),
        old_path: None,
        staged: None,
        unstaged: Some(FileChangeKind::Untracked),
    }];

    assert!(merge_partial_status(&mut status, &["src".into()], incoming));
    assert_eq!(
        status
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["README.md", "src/main.rs"]
    );
}

#[test]
fn partial_status_detects_unchanged_result() {
    let existing = FileStatus {
        path: "src/lib.rs".into(),
        old_path: None,
        staged: None,
        unstaged: Some(FileChangeKind::Modified),
    };
    let mut status = ramag_domain::entities::WorkingTreeStatus {
        files: vec![existing.clone()],
        ..Default::default()
    };

    assert!(!merge_partial_status(
        &mut status,
        &["src/lib.rs".into()],
        vec![existing]
    ));
}

#[test]
fn partial_status_directory_replaces_descendants_without_similar_sibling() {
    let mut status = ramag_domain::entities::WorkingTreeStatus {
        files: vec![
            FileStatus {
                path: "src-old/file.rs".into(),
                old_path: None,
                staged: None,
                unstaged: Some(FileChangeKind::Modified),
            },
            FileStatus {
                path: "src/a.rs".into(),
                old_path: None,
                staged: None,
                unstaged: Some(FileChangeKind::Modified),
            },
            FileStatus {
                path: "src/nested/b.rs".into(),
                old_path: None,
                staged: None,
                unstaged: Some(FileChangeKind::Modified),
            },
        ],
        ..Default::default()
    };
    let incoming = FileStatus {
        path: "src/new.rs".into(),
        old_path: None,
        staged: None,
        unstaged: Some(FileChangeKind::Untracked),
    };

    assert!(merge_partial_status(
        &mut status,
        &["src".into()],
        vec![incoming]
    ));
    assert_eq!(
        status
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src-old/file.rs", "src/new.rs"]
    );
}

#[test]
fn partial_project_files_replace_only_covered_members() {
    let mut files = vec!["README.md".into(), "src/lib.rs".into(), "src/old.rs".into()];

    assert!(merge_partial_project_files(
        &mut files,
        &["src".into()],
        vec!["src/lib.rs".into(), "src/new.rs".into()]
    ));
    assert_eq!(files, ["README.md", "src/lib.rs", "src/new.rs"]);
}

#[test]
fn partial_project_files_keep_unchanged_vector() {
    let mut files = vec!["README.md".into(), "src/lib.rs".into()];

    assert!(!merge_partial_project_files(
        &mut files,
        &["src".into()],
        vec!["src/lib.rs".into()]
    ));
    assert_eq!(files, ["README.md", "src/lib.rs"]);
}

#[test]
fn project_prefix_range_does_not_consume_similar_sibling() {
    let mut files = vec!["src".into(), "src-old/file.rs".into(), "src/a.rs".into()];

    assert!(merge_partial_project_files(
        &mut files,
        &["src".into()],
        vec!["src/new.rs".into()]
    ));
    assert_eq!(files, ["src-old/file.rs", "src/new.rs"]);
}

#[test]
fn partial_rename_removes_the_previous_path_even_if_only_new_path_was_reported() {
    let mut status = ramag_domain::entities::WorkingTreeStatus {
        files: vec![FileStatus {
            path: "old.rs".into(),
            old_path: None,
            staged: None,
            unstaged: Some(FileChangeKind::Modified),
        }],
        ..Default::default()
    };
    let renamed = FileStatus {
        path: "new.rs".into(),
        old_path: Some("old.rs".into()),
        staged: Some(FileChangeKind::Renamed),
        unstaged: None,
    };

    assert!(merge_partial_status(
        &mut status,
        &["new.rs".into()],
        vec![renamed]
    ));
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].path, "new.rs");
    assert_eq!(status.files[0].old_path.as_deref(), Some("old.rs"));
}
