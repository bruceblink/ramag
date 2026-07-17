//! 剪贴图片媒体缓存：按 key 落盘（原始字节，加密由 service 负责）。
//! 缓存目录由 `directories::ProjectDirs` 按当前平台定位。

use std::ffi::OsStr;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ramag_domain::entities::MAX_CLIPBOARD_ITEM_BYTES;
use ramag_domain::error::{DomainError, Result};
use tracing::warn;

/// AES-GCM 媒体格式为 nonce(12) + 明文等长密文 + tag(16)。
const MEDIA_ENCRYPTION_OVERHEAD_BYTES: usize = 12 + 16;
const MAX_MEDIA_BYTES: usize = MAX_CLIPBOARD_ITEM_BYTES as usize + MEDIA_ENCRYPTION_OVERHEAD_BYTES;
const MAX_MEDIA_FILES: usize = 100_000;
const STALE_TEMP_FILE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct MediaStore {
    dir: PathBuf,
}

impl MediaStore {
    pub(crate) fn new() -> Self {
        let dir = directories::ProjectDirs::from("com", "ramag", "ramag")
            .map(|p| p.data_dir().join("clips"))
            .unwrap_or_else(|| std::env::temp_dir().join("ramag-clips"));
        Self { dir }
    }

    /// 按 key 写字节（同名去重，不覆盖）；key 由 service 用内容指纹生成
    pub(crate) fn persist(&self, key: &str, bytes: &[u8]) -> Result<String> {
        if bytes.len() > MAX_MEDIA_BYTES {
            return Err(DomainError::Storage(format!(
                "剪贴媒体超过 {} MiB 安全上限",
                MAX_MEDIA_BYTES / 1024 / 1024
            )));
        }
        let file_name = sanitize(key)
            .ok_or_else(|| DomainError::Storage("剪贴媒体缓存键不是有效文件名".into()))?;
        let path = self.dir.join(file_name);
        fs::create_dir_all(&self.dir)
            .map_err(|e| DomainError::Storage(format!("创建媒体缓存目录失败：{e}")))?;
        set_private_dir_permissions(&self.dir)
            .map_err(|e| DomainError::Storage(format!("收紧媒体缓存目录权限失败：{e}")))?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(DomainError::Storage(
                    "剪贴媒体目标不是可安全复用的普通文件".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.persist_atomic(&path, bytes)?;
            }
            Err(error) => {
                return Err(DomainError::Storage(format!(
                    "检查剪贴媒体目标失败：{error}"
                )));
            }
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| DomainError::Storage(format!("复核剪贴媒体目标失败：{e}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(DomainError::Storage(
                "剪贴媒体目标在写入期间变成了非普通文件".into(),
            ));
        }
        set_private_file_permissions(&path)
            .map_err(|e| DomainError::Storage(format!("收紧剪贴媒体文件权限失败：{e}")))?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn persist_atomic(&self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| DomainError::Storage("剪贴媒体目标文件名无效".into()))?;
        let temp = self.dir.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));

        let write_result = (|| -> std::io::Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)
        })();

        match write_result {
            Ok(()) => Ok(()),
            Err(_error) if path.exists() => {
                remove_temp_file(&temp);
                Ok(())
            }
            Err(error) => {
                remove_temp_file(&temp);
                Err(DomainError::Storage(format!(
                    "原子写入剪贴媒体失败：{error}"
                )))
            }
        }
    }

    /// 读字节（密文，由 service 解密）；仅允许缓存目录内
    pub(crate) fn read(&self, path: &str) -> Result<Vec<u8>> {
        let p = self
            .managed_path(path)
            .ok_or_else(|| DomainError::Storage("拒绝读取媒体目录外文件".into()))?;
        let metadata = fs::symlink_metadata(&p)
            .map_err(|e| DomainError::Storage(format!("读取剪贴媒体元数据失败：{e}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(DomainError::Storage(
                "剪贴媒体不是可安全读取的普通文件".into(),
            ));
        }
        read_limited(&p, MAX_MEDIA_BYTES)
    }

    /// 列出缓存目录全部文件路径（孤儿清理用）
    pub(crate) fn list(&self) -> Result<Vec<String>> {
        self.list_with_limit(MAX_MEDIA_FILES)
    }

    fn list_with_limit(&self, limit: usize) -> Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(format!("读取媒体目录失败：{e}"))),
        };
        let mut out = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    let file_type = match entry.file_type() {
                        Ok(file_type) => file_type,
                        Err(error) => {
                            warn!(error = %error, path = %path.display(), "read media cache file type failed");
                            continue;
                        }
                    };
                    if !file_type.is_file() {
                        continue;
                    }
                    if is_temp_file_name(&entry.file_name()) {
                        cleanup_stale_temp_file(&path);
                        continue;
                    }
                    if out.len() >= limit {
                        return Err(DomainError::Storage(format!(
                            "剪贴媒体缓存文件超过 {limit} 个安全上限，请手动清理 {}",
                            self.dir.display()
                        )));
                    }
                    out.push(path.to_string_lossy().into_owned());
                }
                Err(error) => warn!(error = %error, "read media cache entry failed"),
            }
        }
        Ok(out)
    }

    /// 删除约束在媒体缓存目录内（防御任意路径删除）；文件不存在视为成功
    pub(crate) fn remove(&self, path: &str) -> Result<()> {
        let Some(p) = self.managed_path(path) else {
            warn!(path, "refuse to remove file outside media dir");
            return Ok(());
        };
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DomainError::Storage(format!("删除剪贴媒体失败：{e}"))),
        }
    }

    /// 清空受管目录。逐项删除保持常量内存；符号链接只删除链接本身，不跟随目标。
    pub(crate) fn clear(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(&self.dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(DomainError::Storage(format!(
                    "检查剪贴媒体目录失败：{error}"
                )));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DomainError::Storage(
                "剪贴媒体目录不是可安全清理的普通目录".into(),
            ));
        }

        let entries = fs::read_dir(&self.dir)
            .map_err(|error| DomainError::Storage(format!("读取剪贴媒体目录失败：{error}")))?;
        let mut first_error = None;
        for entry in entries {
            let result = (|| -> std::io::Result<()> {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if !file_type.is_file() && !file_type.is_symlink() {
                    return Err(std::io::Error::other(format!(
                        "拒绝删除非文件条目 {}",
                        entry.path().display()
                    )));
                }
                fs::remove_file(entry.path())
            })();
            if let Err(error) = result {
                warn!(error = %error, "clear media cache entry failed");
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(DomainError::Storage(format!(
                "剪贴媒体目录未能完全清空：{error}"
            ))),
            None => Ok(()),
        }
    }

    fn managed_path(&self, path: &str) -> Option<PathBuf> {
        let path = PathBuf::from(path);
        (path.parent() == Some(self.dir.as_path()) && path.file_name().is_some()).then_some(path)
    }
}

fn read_limited(path: &std::path::Path, limit: usize) -> Result<Vec<u8>> {
    let file =
        fs::File::open(path).map_err(|e| DomainError::Storage(format!("打开剪贴媒体失败：{e}")))?;
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| DomainError::Storage(format!("读取剪贴媒体失败：{e}")))?;
    if bytes.len() > limit {
        return Err(DomainError::Storage(format!(
            "剪贴媒体超过 {} MiB 安全上限",
            limit / 1024 / 1024
        )));
    }
    Ok(bytes)
}

fn remove_temp_file(path: &std::path::Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(error = %error, path = %path.display(), "remove media temp file failed")
        }
    }
}

fn is_temp_file_name(name: &OsStr) -> bool {
    let Some(body) = name
        .to_str()
        .and_then(|name| name.strip_prefix('.'))
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((with_pid, sequence)) = body.rsplit_once('.') else {
        return false;
    };
    let Some((file_name, pid)) = with_pid.rsplit_once('.') else {
        return false;
    };
    !file_name.is_empty() && pid.parse::<u32>().is_ok() && sequence.parse::<u64>().is_ok()
}

fn cleanup_stale_temp_file(path: &std::path::Path) {
    let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) => {
            warn!(error = %error, path = %path.display(), "read media temp file metadata failed");
            return;
        }
    };
    match modified.elapsed() {
        Ok(age) if age >= STALE_TEMP_FILE_AGE => remove_temp_file(path),
        Ok(_) => {}
        Err(error) => {
            warn!(error = %error, path = %path.display(), "media temp file timestamp is in the future")
        }
    }
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// 防目录穿越：只保留文件名部分
fn sanitize(key: &str) -> Option<String> {
    let name = key.rsplit(['/', '\\']).next().unwrap_or(key);
    (!name.is_empty() && name != "." && name != ".." && !name.contains('\0'))
        .then(|| name.to_string())
}

#[cfg(test)]
mod tests {
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
}
