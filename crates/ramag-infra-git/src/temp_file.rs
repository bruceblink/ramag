//! Git 子流程使用的独占临时文件；创建时限制权限，离开作用域自动清理。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ramag_domain::error::{DomainError, Result};

pub(crate) struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub(crate) fn create(prefix: &str, extension: &str, content: &[u8]) -> Result<Self> {
        let tag = nano_id();
        for attempt in 0..16 {
            let path = std::env::temp_dir().join(format!("{prefix}_{tag}_{attempt}.{extension}"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(DomainError::Other(format!(
                        "创建 Git 临时文件 {} 失败: {error}",
                        path.display()
                    )));
                }
            };
            let temp = Self { path };
            if let Err(error) = file.write_all(content) {
                let message = DomainError::Other(format!(
                    "写 Git 临时文件 {} 失败: {error}",
                    temp.path.display()
                ));
                drop(file);
                return Err(message);
            }
            return Ok(temp);
        }
        Err(DomainError::Other(
            "创建 Git 临时文件失败：连续发生文件名冲突".into(),
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    operation = "vcs_temp_file_cleanup",
                    error = %error,
                    path = %self.path.display(),
                    "cleanup git temporary file failed"
                );
            }
        }
    }
}

fn nano_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{ns:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::TempFile;

    #[test]
    fn temporary_file_is_exclusive_and_removed_on_drop() -> ramag_domain::error::Result<()> {
        let path = {
            let temp = TempFile::create("ramag_git_test", "txt", b"content")?;
            let path = temp.path().to_path_buf();
            let content = std::fs::read(&path).map_err(|error| {
                ramag_domain::error::DomainError::Other(format!("读取测试临时文件失败：{error}"))
            })?;
            assert_eq!(content, b"content");
            path
        };
        assert!(!path.exists());
        Ok(())
    }
}
