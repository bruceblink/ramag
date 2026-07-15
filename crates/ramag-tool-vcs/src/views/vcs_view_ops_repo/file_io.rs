//! 文件读盘 + 大文件截断 + 二进制识别 + 主线程 finalize

use super::{PF_FILE_MAX_BYTES, RawFileContent};
use crate::views::helpers::FileContentSnapshot;

/// 读盘失败（路径不存在 / 权限不足）→ raw.error 携带消息，UI 渲染层提示
pub(in crate::views) fn read_raw_file_content(
    repo_root: &std::path::Path,
    rel: &str,
) -> RawFileContent {
    let abs = match resolve_repo_file(repo_root, rel) {
        Ok(path) => path,
        Err(error) => return RawFileContent::with_error(rel.to_string(), error),
    };
    // 不跟随符号链接：打开不可信仓库时不得借 tracked symlink 读取仓库外文件。
    let metadata = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) => {
            return RawFileContent {
                path: rel.to_string(),
                lines: Vec::new(),
                max_chars: 0,
                truncated: false,
                binary: false,
                error: Some(format!("无法访问文件: {e}")),
            };
        }
    };
    if metadata.file_type().is_symlink() {
        return RawFileContent::with_error(
            rel.to_string(),
            "为保护本地文件，Project Files 不读取符号链接".into(),
        );
    }
    if !metadata.is_file() {
        return RawFileContent {
            path: rel.to_string(),
            lines: Vec::new(),
            max_chars: 0,
            truncated: false,
            binary: false,
            error: Some("不是普通文件（可能是软链接 / 设备文件）".into()),
        };
    }
    // 始终按上限读取：文件可在 metadata 后增长，不能让 TOCTOU 绕过 4MB 资源边界。
    let mut bytes = match read_first_bytes(&abs, PF_FILE_MAX_BYTES as usize + 1) {
        Ok(b) => b,
        Err(e) => {
            return RawFileContent {
                path: rel.to_string(),
                lines: Vec::new(),
                max_chars: 0,
                truncated: false,
                binary: false,
                error: Some(format!("读取文件失败: {e}")),
            };
        }
    };
    let truncated = metadata.len() > PF_FILE_MAX_BYTES || bytes.len() > PF_FILE_MAX_BYTES as usize;
    bytes.truncate(PF_FILE_MAX_BYTES as usize);
    // 二进制识别：前 8KB 任一字节为 NUL → 不渲染内容
    let head_len = bytes.len().min(8192);
    if bytes[..head_len].contains(&0) {
        return RawFileContent {
            path: rel.to_string(),
            lines: Vec::new(),
            max_chars: 0,
            truncated: false,
            binary: true,
            error: None,
        };
    }
    let Some(text) = decode_preview_text(bytes, truncated) else {
        return RawFileContent {
            path: rel.to_string(),
            lines: Vec::new(),
            max_chars: 0,
            truncated,
            binary: true,
            error: None,
        };
    };
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let max_chars = lines
        .iter()
        .map(|line| super::super::syntax::display_cols(line))
        .max()
        .unwrap_or(0);
    RawFileContent {
        path: rel.to_string(),
        lines,
        max_chars,
        truncated,
        binary: false,
        error: None,
    }
}

fn resolve_repo_file(repo_root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    use std::path::Component;

    if rel.is_empty() {
        return Err("文件路径为空".into());
    }
    let rel_path = std::path::Path::new(rel);
    for component in rel_path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => return Err("文件路径包含冗余的当前目录段".into()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("文件路径试图越出仓库目录".into());
            }
        }
    }
    Ok(repo_root.join(rel_path))
}

fn decode_preview_text(bytes: Vec<u8>, truncated: bool) -> Option<String> {
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) => {
            let utf8_error = error.utf8_error();
            if !truncated || utf8_error.error_len().is_some() {
                return None;
            }
            // 大文件可能恰好截在多字节字符中；仅移除末尾不完整字符，内部坏字节仍判二进制。
            let valid_up_to = utf8_error.valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_up_to);
            String::from_utf8(bytes).ok()
        }
    }
}

/// 主线程 finalize：仅包 Rc；列宽已在 worker 线程计算
pub(super) fn finalize_file_snapshot(raw: RawFileContent) -> FileContentSnapshot {
    FileContentSnapshot {
        path: raw.path,
        lines: std::rc::Rc::new(raw.lines),
        max_chars: raw.max_chars,
        truncated: raw.truncated,
        binary: raw.binary,
        error: raw.error,
    }
}

/// 读取文件前 `limit` 字节（用于大文件截断预览）
fn read_first_bytes(path: &std::path::Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::{decode_preview_text, read_raw_file_content, resolve_repo_file};

    #[test]
    fn repository_file_path_cannot_escape_root() {
        let root = std::path::Path::new("/repo");
        assert_eq!(
            resolve_repo_file(root, "src/main.rs").ok(),
            Some(root.join("src/main.rs"))
        );
        assert!(resolve_repo_file(root, "../secret").is_err());
        assert!(resolve_repo_file(root, "/etc/passwd").is_err());
    }

    #[test]
    fn truncated_utf8_tail_is_safe_but_internal_invalid_byte_is_binary() {
        assert_eq!(
            decode_preview_text(vec![b'a', 0xe4, 0xb8], true).as_deref(),
            Some("a")
        );
        assert!(decode_preview_text(vec![b'a', 0xff, b'b'], false).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_is_not_followed() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ramag-vcs-file-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let outside = root.with_extension("secret");
        std::fs::write(&outside, b"secret")?;
        symlink(&outside, root.join("link"))?;

        let raw = read_raw_file_content(&root, "link");
        assert!(raw.error.is_some());
        assert!(raw.lines.is_empty());

        std::fs::remove_file(root.join("link"))?;
        std::fs::remove_dir_all(root)?;
        std::fs::remove_file(outside)?;
        Ok(())
    }
}
