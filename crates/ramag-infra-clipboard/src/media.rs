//! 剪贴图片媒体缓存：按 key 落盘（原始字节，加密由 service 负责）。
//! 缓存目录由 `directories::ProjectDirs` 按当前平台定位。

use std::fs;
use std::path::PathBuf;

use ramag_domain::error::{DomainError, Result};
use tracing::warn;

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
        let file_name = sanitize(key)
            .ok_or_else(|| DomainError::Storage("剪贴媒体缓存键不是有效文件名".into()))?;
        let path = self.dir.join(file_name);
        if !path.exists() {
            fs::create_dir_all(&self.dir)
                .map_err(|e| DomainError::Storage(format!("创建媒体缓存目录失败：{e}")))?;
            fs::write(&path, bytes)
                .map_err(|e| DomainError::Storage(format!("写入剪贴媒体失败：{e}")))?;
        }
        Ok(path.to_string_lossy().into_owned())
    }

    /// 读字节（密文，由 service 解密）；仅允许缓存目录内
    pub(crate) fn read(&self, path: &str) -> Result<Vec<u8>> {
        let p = self
            .managed_path(path)
            .ok_or_else(|| DomainError::Storage("拒绝读取媒体目录外文件".into()))?;
        fs::read(&p).map_err(|e| DomainError::Storage(format!("读取剪贴媒体失败：{e}")))
    }

    /// 列出缓存目录全部文件路径（孤儿清理用）
    pub(crate) fn list(&self) -> Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DomainError::Storage(format!("读取媒体目录失败：{e}"))),
        };
        let mut out = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) if entry.path().is_file() => {
                    out.push(entry.path().to_string_lossy().into_owned());
                }
                Ok(_) => {}
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

    fn managed_path(&self, path: &str) -> Option<PathBuf> {
        let path = PathBuf::from(path);
        (path.parent() == Some(self.dir.as_path()) && path.file_name().is_some()).then_some(path)
    }
}

/// 防目录穿越：只保留文件名部分
fn sanitize(key: &str) -> Option<String> {
    let name = key.rsplit(['/', '\\']).next().unwrap_or(key);
    (!name.is_empty() && name != "." && name != ".." && !name.contains('\0'))
        .then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn cache_key_keeps_only_safe_file_name() {
        assert_eq!(sanitize("abc.img"), Some("abc.img".into()));
        assert_eq!(sanitize(r"folder\abc.img"), Some("abc.img".into()));
        assert_eq!(sanitize("../"), None);
        assert_eq!(sanitize(".."), None);
        assert_eq!(sanitize("a\0b"), None);
    }
}
