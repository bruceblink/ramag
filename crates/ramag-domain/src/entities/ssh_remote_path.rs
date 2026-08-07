//! 远端 SFTP 路径值对象；解析规则与本机操作系统无关。

use serde::{Deserialize, Serialize};

use super::ssh::MAX_SSH_PATH_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SftpNamespaceKind {
    Posix,
    WindowsDrive,
    Virtual,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemotePath {
    canonical: String,
    namespace: SftpNamespaceKind,
}

impl RemotePath {
    /// 解析服务器返回的规范路径。`/` 默认按 POSIX 处理；Windows 虚拟根需显式给出命名空间。
    pub fn parse_server_canonical(value: &str) -> Result<Self, String> {
        let namespace = if is_windows_drive_absolute(value) {
            SftpNamespaceKind::WindowsDrive
        } else if has_virtual_windows_drive_prefix(value) {
            SftpNamespaceKind::Virtual
        } else if value.starts_with('/') {
            SftpNamespaceKind::Posix
        } else {
            return Err("服务器返回的远程路径不是受支持的绝对路径".into());
        };
        Self::parse_with_namespace(value, namespace)
    }

    pub fn parse_with_namespace(value: &str, namespace: SftpNamespaceKind) -> Result<Self, String> {
        validate_protocol_path(value)?;
        match namespace {
            SftpNamespaceKind::Posix => validate_posix_absolute(value)?,
            SftpNamespaceKind::Virtual => validate_virtual_absolute(value)?,
            SftpNamespaceKind::WindowsDrive => validate_windows_drive_absolute(value)?,
            SftpNamespaceKind::Unknown => {
                return Err("远程路径命名空间尚未识别".into());
            }
        }
        Ok(Self {
            canonical: normalize_trailing_separator(value, namespace),
            namespace,
        })
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn namespace(&self) -> SftpNamespaceKind {
        self.namespace
    }

    pub fn is_root(&self) -> bool {
        match self.namespace {
            SftpNamespaceKind::Posix => self.canonical == "/",
            SftpNamespaceKind::Virtual => {
                self.canonical == "/" || is_virtual_windows_drive_root(&self.canonical)
            }
            SftpNamespaceKind::WindowsDrive => {
                self.canonical.len() == 3 && self.canonical.as_bytes()[2] == b'/'
            }
            SftpNamespaceKind::Unknown => false,
        }
    }

    pub fn join_child(&self, name: &str) -> Result<Self, String> {
        let name_namespace = if self.namespace == SftpNamespaceKind::Virtual
            && has_virtual_windows_drive_prefix(&self.canonical)
        {
            SftpNamespaceKind::WindowsDrive
        } else {
            self.namespace
        };
        validate_remote_name_for_namespace(name, name_namespace)?;
        let value = match self.namespace {
            SftpNamespaceKind::Posix if self.is_root() => format!("/{name}"),
            SftpNamespaceKind::Virtual if self.canonical == "/" => format!("/{name}"),
            SftpNamespaceKind::WindowsDrive if self.is_root() => {
                format!("{}{name}", self.canonical)
            }
            SftpNamespaceKind::Virtual if self.canonical.ends_with('/') => {
                format!("{}{name}", self.canonical)
            }
            SftpNamespaceKind::Unknown => return Err("远程路径命名空间尚未识别".into()),
            _ => format!("{}/{name}", self.canonical),
        };
        Self::parse_with_namespace(&value, self.namespace)
    }

    pub fn parent(&self) -> Self {
        if self.namespace == SftpNamespaceKind::Virtual
            && is_virtual_windows_drive_root(&self.canonical)
        {
            return Self {
                canonical: "/".into(),
                namespace: self.namespace,
            };
        }
        if self.is_root() {
            return self.clone();
        }
        let index = self.canonical.rfind('/').unwrap_or_default();
        let parent = match self.namespace {
            SftpNamespaceKind::Posix | SftpNamespaceKind::Virtual if index == 0 => "/",
            SftpNamespaceKind::Virtual
                if index == 3 && has_virtual_windows_drive_prefix(&self.canonical) =>
            {
                &self.canonical[..=index]
            }
            SftpNamespaceKind::WindowsDrive if index == 2 => &self.canonical[..=2],
            _ => &self.canonical[..index],
        };
        Self {
            canonical: parent.to_string(),
            namespace: self.namespace,
        }
    }

    pub fn is_same_location(&self, other: &Self) -> bool {
        self.namespace == other.namespace && self.canonical == other.canonical
    }

    pub fn temporary_sibling(&self, marker: &str, unique: &str) -> Result<Self, String> {
        if self.is_root() {
            return Err("不能为远端根目录创建临时兄弟路径".into());
        }
        validate_portable_token("临时文件标记", marker, 32)?;
        validate_portable_token("临时文件标识", unique, 64)?;
        let name = self
            .canonical
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "远程目标缺少文件名".to_string())?;
        self.parent()
            .join_child(&format!(".{name}.{marker}-{unique}.tmp"))
    }

    pub fn breadcrumbs(&self) -> Vec<(String, Self)> {
        let mut result = Vec::new();
        match self.namespace {
            SftpNamespaceKind::Posix => {
                let root = Self {
                    canonical: "/".into(),
                    namespace: self.namespace,
                };
                result.push(("/".into(), root.clone()));
                let mut current = root;
                for component in self.canonical.trim_start_matches('/').split('/') {
                    if component.is_empty() {
                        continue;
                    }
                    let canonical = if current.is_root() {
                        format!("/{component}")
                    } else {
                        format!("{}/{component}", current.canonical)
                    };
                    current = Self {
                        canonical,
                        namespace: self.namespace,
                    };
                    result.push((component.to_string(), current.clone()));
                }
            }
            SftpNamespaceKind::Virtual => {
                let root = Self {
                    canonical: "/".into(),
                    namespace: self.namespace,
                };
                result.push(("/".into(), root.clone()));
                let (mut current, tail) = if has_virtual_windows_drive_prefix(&self.canonical) {
                    let drive = Self {
                        canonical: self.canonical[..4].into(),
                        namespace: self.namespace,
                    };
                    result.push((self.canonical[1..3].into(), drive.clone()));
                    (drive, &self.canonical[4..])
                } else {
                    (root, self.canonical.trim_start_matches('/'))
                };
                for component in tail.split('/').filter(|component| !component.is_empty()) {
                    let canonical = if current.canonical.ends_with('/') {
                        format!("{}{component}", current.canonical)
                    } else {
                        format!("{}/{component}", current.canonical)
                    };
                    current = Self {
                        canonical,
                        namespace: self.namespace,
                    };
                    result.push((component.to_string(), current.clone()));
                }
            }
            SftpNamespaceKind::WindowsDrive => {
                let root_value = &self.canonical[..3];
                let root = Self {
                    canonical: root_value.into(),
                    namespace: self.namespace,
                };
                result.push((root_value.into(), root.clone()));
                let mut current = root;
                for component in self.canonical[3..].split('/') {
                    if component.is_empty() {
                        continue;
                    }
                    let canonical = if current.is_root() {
                        format!("{}{component}", current.canonical)
                    } else {
                        format!("{}/{component}", current.canonical)
                    };
                    current = Self {
                        canonical,
                        namespace: self.namespace,
                    };
                    result.push((component.to_string(), current.clone()));
                }
            }
            SftpNamespaceKind::Unknown => {}
        }
        result
    }
}

impl std::fmt::Display for RemotePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.canonical)
    }
}

pub fn infer_sftp_namespace(path: &str) -> SftpNamespaceKind {
    if is_windows_drive_absolute(path) {
        SftpNamespaceKind::WindowsDrive
    } else if has_virtual_windows_drive_prefix(path) {
        SftpNamespaceKind::Virtual
    } else if path.starts_with('/') {
        SftpNamespaceKind::Posix
    } else {
        SftpNamespaceKind::Unknown
    }
}

pub fn validate_remote_name_for_namespace(
    name: &str,
    namespace: SftpNamespaceKind,
) -> Result<(), String> {
    validate_protocol_path(name)?;
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
        return Err("远程文件名不能包含路径分隔符或使用 . / ..".into());
    }
    if matches!(namespace, SftpNamespaceKind::WindowsDrive) {
        if name.contains(['<', '>', ':', '"', '|', '?', '*']) {
            return Err("Windows SFTP 文件名包含系统不允许的字符".into());
        }
        if name.ends_with([' ', '.']) {
            return Err("Windows SFTP 文件名不能以空格或点结尾".into());
        }
        let stem = name.split('.').next().unwrap_or_default();
        if is_windows_reserved_name(stem) {
            return Err("Windows SFTP 文件名使用了保留设备名".into());
        }
    }
    Ok(())
}

fn validate_protocol_path(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("远程路径不能为空".into());
    }
    if value.len() > MAX_SSH_PATH_BYTES {
        return Err(format!(
            "远程路径过长：{} bytes，最多 {MAX_SSH_PATH_BYTES} bytes",
            value.len()
        ));
    }
    if value.chars().any(char::is_control) {
        return Err("远程路径不能包含控制字符".into());
    }
    Ok(())
}

fn validate_posix_absolute(value: &str) -> Result<(), String> {
    if !value.starts_with('/') || (value.len() > 1 && value.contains("//")) || value.contains('\\')
    {
        return Err("远程 POSIX 路径必须以单个 '/' 开头".into());
    }
    validate_components(value.trim_matches('/'))
}

fn validate_virtual_absolute(value: &str) -> Result<(), String> {
    validate_posix_absolute(value)?;
    if has_virtual_windows_drive_prefix(value) {
        for component in value[4..]
            .split('/')
            .filter(|component| !component.is_empty())
        {
            validate_remote_name_for_namespace(component, SftpNamespaceKind::WindowsDrive)?;
        }
    }
    Ok(())
}

fn validate_windows_drive_absolute(value: &str) -> Result<(), String> {
    if !is_windows_drive_absolute(value) {
        return Err("Windows SFTP 路径必须使用 C:/path 形式的盘符绝对路径".into());
    }
    if value.contains('\\') || value.starts_with("//") || value.starts_with("\\\\") {
        return Err("Windows SFTP 暂不支持 UNC 或设备路径".into());
    }
    let tail = &value[3..];
    if tail.contains("//") {
        return Err("Windows SFTP 规范路径不能包含空组件".into());
    }
    validate_components(tail.trim_matches('/'))?;
    for component in tail.split('/').filter(|component| !component.is_empty()) {
        validate_remote_name_for_namespace(component, SftpNamespaceKind::WindowsDrive)?;
    }
    Ok(())
}

fn validate_components(value: &str) -> Result<(), String> {
    if value
        .split('/')
        .any(|component| matches!(component, "." | ".."))
    {
        return Err("规范远程路径不能包含 . 或 .. 组件".into());
    }
    Ok(())
}

fn normalize_trailing_separator(value: &str, namespace: SftpNamespaceKind) -> String {
    let is_root = value == "/"
        || (namespace == SftpNamespaceKind::WindowsDrive
            && value.len() == 3
            && value.as_bytes()[2] == b'/')
        || (namespace == SftpNamespaceKind::Virtual && is_virtual_windows_drive_root(value));
    if is_root {
        value.to_string()
    } else {
        value.trim_end_matches('/').to_string()
    }
}

fn has_virtual_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes[3] == b'/'
}

fn is_virtual_windows_drive_root(value: &str) -> bool {
    value.len() == 4 && has_virtual_windows_drive_prefix(value)
}

fn is_windows_drive_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn is_windows_reserved_name(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || upper.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
}

fn validate_portable_token(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(format!("{label}格式无效"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_paths_support_parent_join_temporary_and_breadcrumbs() -> Result<(), String> {
        let path = RemotePath::parse_server_canonical("/var/log/app.log")?;
        assert_eq!(path.namespace(), SftpNamespaceKind::Posix);
        assert_eq!(path.parent().canonical(), "/var/log");
        assert_eq!(
            path.temporary_sibling("ramag-edit", "123")?.canonical(),
            "/var/log/.app.log.ramag-edit-123.tmp"
        );
        assert_eq!(
            path.breadcrumbs()
                .into_iter()
                .map(|(_, path)| path.canonical().to_string())
                .collect::<Vec<_>>(),
            ["/", "/var", "/var/log", "/var/log/app.log"]
        );
        Ok(())
    }

    #[test]
    fn windows_drive_paths_preserve_spelling_and_protect_drive_root() -> Result<(), String> {
        let root = RemotePath::parse_server_canonical("d:/")?;
        assert!(root.is_root());
        assert_eq!(root.parent().canonical(), "d:/");
        let child = root.join_child("中文 Data")?;
        assert_eq!(child.canonical(), "d:/中文 Data");
        assert_eq!(child.parent(), root);
        Ok(())
    }

    #[test]
    fn windows_paths_reject_unsafe_or_ambiguous_forms() {
        for path in [
            "C:relative",
            "//server/share",
            r"\\server\share",
            "C:/dir/../secret",
            "C:/file.txt:stream",
            "C:/CON.txt",
            "C:/trailing. ",
        ] {
            assert!(RemotePath::parse_server_canonical(path).is_err(), "{path}");
        }
    }

    #[test]
    fn virtual_root_is_distinct_from_posix_root() -> Result<(), String> {
        let virtual_root = RemotePath::parse_with_namespace("/", SftpNamespaceKind::Virtual)?;
        let posix_root = RemotePath::parse_server_canonical("/")?;
        assert!(virtual_root.is_root());
        assert!(!virtual_root.is_same_location(&posix_root));
        Ok(())
    }

    #[test]
    fn virtual_windows_drives_keep_the_drive_root_and_breadcrumb() -> Result<(), String> {
        let root = RemotePath::parse_with_namespace("/C:/", SftpNamespaceKind::Virtual)?;
        assert!(root.is_root());
        assert_eq!(root.parent().canonical(), "/");
        let child = root.join_child("Program Files")?;
        assert_eq!(child.canonical(), "/C:/Program Files");
        assert_eq!(child.parent(), root);
        assert_eq!(
            child
                .breadcrumbs()
                .into_iter()
                .map(|(label, path)| (label, path.to_string()))
                .collect::<Vec<_>>(),
            [
                ("/".into(), "/".into()),
                ("C:".into(), "/C:/".into()),
                ("Program Files".into(), "/C:/Program Files".into()),
            ]
        );
        Ok(())
    }
}
