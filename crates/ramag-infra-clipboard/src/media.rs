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
        self.ensure_regular_dir()?;
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
            fs::hard_link(&temp, path)
        })();

        match write_result {
            Ok(()) => {
                remove_temp_file(&temp);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
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
        self.ensure_regular_dir()?;
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
        match fs::symlink_metadata(&self.dir) {
            Ok(_) => self.ensure_regular_dir()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(DomainError::Storage(format!(
                    "检查剪贴媒体目录失败：{error}"
                )));
            }
        }
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
        match fs::symlink_metadata(&self.dir) {
            Ok(_) => self.ensure_regular_dir()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(DomainError::Storage(format!(
                    "检查剪贴媒体目录失败：{error}"
                )));
            }
        }
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

    fn ensure_regular_dir(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.dir)
            .map_err(|error| DomainError::Storage(format!("检查剪贴媒体目录失败：{error}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DomainError::Storage(
                "剪贴媒体目录不是可安全使用的普通目录".into(),
            ));
        }
        Ok(())
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
mod tests;
