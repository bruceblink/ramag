//! VCS 仓库管理与克隆清理测试。

use super::{
    MAX_OPEN_REPOS, MAX_OPEN_REPOS_PREF_BYTES, parse_open_repo_paths, prepare_clone_destination,
    remove_cancelled_clone_directory,
};
use std::sync::atomic::{AtomicU64, Ordering};

fn test_root(label: &str) -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "ramag-vcs-clone-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn clone_destination_must_not_preexist() -> std::io::Result<()> {
    let root = test_root("existing");
    let target = root.join("repo");
    std::fs::create_dir_all(&target)?;
    std::fs::write(target.join("keep.txt"), b"keep")?;

    assert!(prepare_clone_destination(&target).is_err());
    assert!(target.join("keep.txt").exists());

    std::fs::remove_dir_all(root)
}

#[test]
fn cleanup_removes_empty_owned_directory() -> std::io::Result<()> {
    let root = test_root("empty");
    let target = root.join("repo");
    std::fs::create_dir_all(&target)?;

    assert!(remove_cancelled_clone_directory(&target).is_ok());
    assert!(!target.exists());

    std::fs::remove_dir(root)
}

#[test]
fn cleanup_refuses_changed_non_git_directory() -> std::io::Result<()> {
    let root = test_root("changed");
    let target = root.join("repo");
    std::fs::create_dir_all(&target)?;
    std::fs::write(target.join("user-file.txt"), b"keep")?;

    assert!(remove_cancelled_clone_directory(&target).is_err());
    assert!(target.join("user-file.txt").exists());

    std::fs::remove_dir_all(root)
}

#[test]
fn cleanup_removes_partial_git_clone() -> std::io::Result<()> {
    let root = test_root("partial");
    let target = root.join("repo");
    std::fs::create_dir_all(target.join(".git"))?;
    std::fs::write(target.join("partial.txt"), b"partial")?;

    assert!(remove_cancelled_clone_directory(&target).is_ok());
    assert!(!target.exists());

    std::fs::remove_dir(root)
}

#[cfg(unix)]
#[test]
fn cleanup_refuses_symbolic_link() -> std::io::Result<()> {
    use std::os::unix::fs::symlink;

    let root = test_root("symlink");
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&outside)?;
    std::fs::create_dir_all(&root)?;
    let target = root.join("repo");
    symlink(&outside, &target)?;

    assert!(remove_cancelled_clone_directory(&target).is_err());
    assert!(outside.exists());

    std::fs::remove_file(target)?;
    std::fs::remove_dir(root)?;
    std::fs::remove_dir(outside)
}

#[test]
fn open_repo_paths_are_bounded_and_deduplicated() {
    let mut paths = vec!["/repo/0".to_string(), "/repo/0".to_string()];
    paths.extend((1..=MAX_OPEN_REPOS).map(|index| format!("/repo/{index}")));
    let json = serde_json::to_string(&paths).unwrap_or_default();

    assert!(matches!(
        parse_open_repo_paths(&json),
        Ok((paths, true))
            if paths.len() == MAX_OPEN_REPOS
                && paths.first().is_some_and(|path| path == "/repo/0")
    ));
    assert!(parse_open_repo_paths(&" ".repeat(MAX_OPEN_REPOS_PREF_BYTES + 1)).is_err());
}
