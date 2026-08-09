use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramag_domain::error::DomainError;

use super::{
    MAX_MEDIA_BYTES, MEDIA_ENCRYPTION_OVERHEAD_BYTES, MediaStore, STALE_TEMP_FILE_AGE,
    is_temp_file_name, read_limited, sanitize,
};
use ramag_domain::entities::MAX_CLIPBOARD_ITEM_BYTES;

#[test]
fn media_limit_includes_encryption_overhead() {
    assert_eq!(
        MAX_MEDIA_BYTES,
        MAX_CLIPBOARD_ITEM_BYTES as usize + MEDIA_ENCRYPTION_OVERHEAD_BYTES
    );
}

#[test]
fn cache_key_keeps_only_safe_file_name() {
    assert_eq!(sanitize("abc.img"), Some("abc.img".into()));
    assert_eq!(sanitize(r"folder\abc.img"), Some("abc.img".into()));
    assert_eq!(sanitize("../"), None);
    assert_eq!(sanitize(".."), None);
    assert_eq!(sanitize("a\0b"), None);
}

#[test]
fn persist_is_atomic_and_keeps_existing_content() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ramag-media-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let store = MediaStore { dir: dir.clone() };

    let path = store.persist("same.img", b"first")?;
    store.persist("same.img", b"second")?;

    assert_eq!(store.read(&path)?, b"first");
    let entries = store.list()?;
    assert_eq!(entries, vec![path]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let dir_mode = std::fs::metadata(&dir)
            .map_err(|error| DomainError::Storage(format!("读取测试目录权限失败：{error}")))?
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&entries[0])
            .map_err(|error| DomainError::Storage(format!("读取测试文件权限失败：{error}")))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }
    std::fs::remove_dir_all(dir)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}

#[test]
fn atomic_publish_does_not_replace_existing_file() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ramag-media-publish-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| DomainError::Storage(format!("创建测试目录失败：{error}")))?;
    let path = dir.join("same.img");
    std::fs::write(&path, b"first")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;

    MediaStore { dir: dir.clone() }.persist_atomic(&path, b"second")?;

    assert_eq!(
        std::fs::read(&path)
            .map_err(|error| DomainError::Storage(format!("读取测试文件失败：{error}")))?,
        b"first"
    );
    std::fs::remove_dir_all(dir)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}

#[test]
fn media_read_limit_is_enforced() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ramag-media-limit-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, b"12345")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    assert!(read_limited(&path, 4).is_err());
    assert_eq!(read_limited(&path, 5)?, b"12345");
    std::fs::remove_file(path)
        .map_err(|error| DomainError::Storage(format!("清理测试文件失败：{error}")))?;
    Ok(())
}

#[test]
fn media_clear_streams_all_regular_files() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ramag-media-clear-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| DomainError::Storage(format!("创建测试目录失败：{error}")))?;
    std::fs::write(dir.join("one.img"), b"1")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    std::fs::write(dir.join("two.img"), b"2")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;

    MediaStore { dir: dir.clone() }.clear()?;

    let remaining = std::fs::read_dir(&dir)
        .map_err(|error| DomainError::Storage(format!("读取测试目录失败：{error}")))?
        .count();
    assert_eq!(remaining, 0);
    std::fs::remove_dir(dir)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn media_clear_removes_symlink_without_following_target() -> ramag_domain::error::Result<()> {
    use std::os::unix::fs::symlink;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "ramag-media-clear-link-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let dir = root.join("media");
    let target = root.join("keep.txt");
    std::fs::create_dir_all(&dir)
        .map_err(|error| DomainError::Storage(format!("创建测试目录失败：{error}")))?;
    std::fs::write(&target, b"keep")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    symlink(&target, dir.join("linked.img"))
        .map_err(|error| DomainError::Storage(format!("创建测试链接失败：{error}")))?;

    MediaStore { dir }.clear()?;

    assert_eq!(
        std::fs::read(&target)
            .map_err(|error| DomainError::Storage(format!("读取目标文件失败：{error}")))?,
        b"keep"
    );
    std::fs::remove_dir_all(root)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}

#[test]
fn media_list_skips_active_temp_and_removes_stale_temp() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ramag-media-list-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| DomainError::Storage(format!("创建测试目录失败：{error}")))?;
    let final_path = dir.join("final.img");
    let active_temp = dir.join(format!(".final.img.{}.1.tmp", std::process::id()));
    let stale_temp = dir.join(format!(".final.img.{}.2.tmp", std::process::id()));
    std::fs::write(&final_path, b"final")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    std::fs::write(&active_temp, b"active")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    let stale_file = std::fs::File::create(&stale_temp)
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    stale_file
        .set_modified(
            std::time::SystemTime::now()
                .checked_sub(STALE_TEMP_FILE_AGE + Duration::from_secs(1))
                .ok_or_else(|| DomainError::Storage("测试时间下溢".into()))?,
        )
        .map_err(|error| DomainError::Storage(format!("设置测试时间失败：{error}")))?;

    let store = MediaStore { dir: dir.clone() };
    assert_eq!(store.list()?, vec![final_path.to_string_lossy()]);
    assert!(active_temp.exists());
    assert!(!stale_temp.exists());
    assert!(is_temp_file_name(
        active_temp.file_name().unwrap_or_default()
    ));
    assert!(!is_temp_file_name(
        final_path.file_name().unwrap_or_default()
    ));

    std::fs::remove_dir_all(dir)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}

#[test]
fn media_list_file_count_is_bounded() -> ramag_domain::error::Result<()> {
    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ramag-media-count-test-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|error| DomainError::Storage(format!("创建测试目录失败：{error}")))?;
    std::fs::write(dir.join("one.img"), b"1")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    std::fs::write(dir.join("two.img"), b"2")
        .map_err(|error| DomainError::Storage(format!("写测试文件失败：{error}")))?;
    let store = MediaStore { dir: dir.clone() };

    assert!(store.list_with_limit(1).is_err());

    std::fs::remove_dir_all(dir)
        .map_err(|error| DomainError::Storage(format!("清理测试目录失败：{error}")))?;
    Ok(())
}
