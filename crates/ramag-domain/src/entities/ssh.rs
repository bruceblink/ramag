//! SSH 配置、远程文件与传输任务实体。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_SSH_PROFILES: usize = 1024;
pub const MAX_SSH_PROFILE_NAME_BYTES: usize = 256;
pub const MAX_SSH_HOST_BYTES: usize = 1024;
pub const MAX_SSH_USERNAME_BYTES: usize = 1024;
pub const MAX_SSH_PATH_BYTES: usize = 32 * 1024;
pub const MAX_REMOTE_DIRECTORY_ENTRIES: usize = 100_000;
pub const MAX_REMOTE_DIRECTORY_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REMOTE_DELETE_DEPTH: usize = 64;
pub const MAX_REMOTE_DELETE_ENTRIES: usize = 100_000;
pub const MAX_REMOTE_DELETE_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TRANSFER_HISTORY: usize = 100;
pub const MAX_QUEUED_TRANSFERS: usize = 64;
pub const MAX_CONCURRENT_TRANSFERS: usize = 3;
pub const MAX_SSH_WORKSPACES: usize = 16;
pub const MAX_SSH_TERMINALS_PER_WORKSPACE: usize = 8;
pub const TRANSFER_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshProfileId(pub Uuid);

impl SshProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SshProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SshProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SshAuthMode {
    #[default]
    System,
    KeyFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfile {
    pub id: SshProfileId,
    pub name: String,
    /// `#RRGGBB`，只用于连接列表与标签识别。
    pub color: String,
    /// 主机名、IP 或 `~/.ssh/config` 别名。
    pub host: String,
    pub port: u16,
    /// 留空时交给 OpenSSH 配置解析。
    pub username: String,
    pub auth_mode: SshAuthMode,
    /// 只保存绝对路径，不读取或复制私钥内容。
    pub key_path: Option<String>,
    /// 空值表示由 SFTP canonicalize(".") 解析远端默认目录。
    pub initial_directory: Option<String>,
    /// 自定义 OpenSSH 可执行文件必须是绝对路径。
    pub ssh_path: Option<String>,
}

impl SshProfile {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            id: SshProfileId::new(),
            name: name.into(),
            color: "#007ACC".into(),
            host: host.into(),
            port: 22,
            username: String::new(),
            auth_mode: SshAuthMode::System,
            key_path: None,
            initial_directory: None,
            ssh_path: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_required_single_line("连接名称", &self.name, MAX_SSH_PROFILE_NAME_BYTES)?;
        validate_color(&self.color)?;
        validate_required_single_line("主机或 SSH 别名", &self.host, MAX_SSH_HOST_BYTES)?;
        if self.host.starts_with('-') {
            return Err("主机或 SSH 别名不能以 '-' 开头".into());
        }
        if self.host.chars().any(char::is_whitespace) {
            return Err("主机或 SSH 别名不能包含空白字符".into());
        }
        if self.port == 0 {
            return Err("SSH 端口必须是 1 - 65535".into());
        }
        validate_optional_single_line("用户名", Some(&self.username), MAX_SSH_USERNAME_BYTES)?;
        if self.username.chars().any(char::is_whitespace) {
            return Err("用户名不能包含空白字符".into());
        }

        match self.auth_mode {
            SshAuthMode::System if self.key_path.is_some() => {
                return Err("系统 SSH 配置 / Agent 认证不能同时指定密钥文件".into());
            }
            SshAuthMode::KeyFile => {
                let key_path = self
                    .key_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| "密钥文件认证必须选择密钥路径".to_string())?;
                validate_absolute_local_path("密钥路径", key_path)?;
                if key_path.to_ascii_lowercase().ends_with(".ppk") {
                    return Err("首个版本不支持 PuTTY .ppk 密钥，请转换为 OpenSSH 格式".into());
                }
            }
            SshAuthMode::System => {}
        }

        if let Some(path) = self.initial_directory.as_deref() {
            validate_initial_remote_path(path)?;
        }
        if let Some(path) = self.ssh_path.as_deref() {
            validate_absolute_local_path("OpenSSH 可执行文件路径", path)?;
        }
        Ok(())
    }

    pub fn initial_path(&self) -> &str {
        self.initial_directory.as_deref().unwrap_or(".")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCapability {
    pub executable: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshLaunchCommand {
    pub profile_id: SshProfileId,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub kind: RemoteEntryKind,
    pub size: u64,
    pub permissions: Option<u32>,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDirectory {
    pub path: String,
    pub entries: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(pub Uuid);

impl TransferId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TransferId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferStatus {
    Waiting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TransferStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwritePolicy {
    Refuse,
    Overwrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferTask {
    pub id: TransferId,
    pub profile_id: SshProfileId,
    pub direction: TransferDirection,
    pub local_path: String,
    pub remote_path: String,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub status: TransferStatus,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TransferTask {
    pub fn new(
        profile_id: SshProfileId,
        direction: TransferDirection,
        local_path: impl Into<String>,
        remote_path: impl Into<String>,
    ) -> Self {
        Self {
            id: TransferId::new(),
            profile_id,
            direction,
            local_path: local_path.into(),
            remote_path: remote_path.into(),
            transferred_bytes: 0,
            total_bytes: 0,
            status: TransferStatus::Waiting,
            error: None,
            created_at: Utc::now(),
            finished_at: None,
        }
    }

    pub fn mark_running(&mut self) {
        if self.status == TransferStatus::Waiting {
            self.status = TransferStatus::Running;
        }
    }

    pub fn update_progress(&mut self, transferred: u64, total: u64) {
        if self.status == TransferStatus::Running {
            self.total_bytes = total;
            self.transferred_bytes = if total == 0 {
                transferred
            } else {
                transferred.min(total)
            };
        }
    }

    pub fn finish(&mut self, result: Result<(), String>, cancelled: bool) {
        if self.status.is_terminal() {
            return;
        }
        self.status = if cancelled {
            TransferStatus::Cancelled
        } else if result.is_ok() {
            TransferStatus::Completed
        } else {
            TransferStatus::Failed
        };
        self.error = result.err();
        self.finished_at = Some(Utc::now());
    }
}

#[derive(Debug, Clone, Default)]
pub struct TransferCancellation(Arc<AtomicBool>);

impl TransferCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub type SshProgressFn = Arc<dyn Fn(u64, u64) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshWorkspaceState {
    pub profile_id: SshProfileId,
    pub last_remote_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SshWorkspacePreference {
    pub workspaces: Vec<SshWorkspaceState>,
    pub active_profile_id: Option<SshProfileId>,
}

pub fn validate_remote_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("远程路径不能为空".into());
    }
    validate_protocol_text("远程路径", path, MAX_SSH_PATH_BYTES)?;
    if path.chars().any(char::is_control) {
        return Err("远程路径不能包含控制字符".into());
    }
    Ok(())
}

pub fn validate_remote_name(name: &str) -> Result<(), String> {
    validate_required_single_line("远程文件名", name, MAX_SSH_PATH_BYTES)?;
    if matches!(name, "." | "..") || name.contains('/') || name.contains('\\') {
        return Err("远程文件名不能包含路径分隔符或使用 . / ..".into());
    }
    Ok(())
}

pub fn join_remote_path(parent: &str, name: &str) -> Result<String, String> {
    validate_remote_path(parent)?;
    validate_remote_name(name)?;
    let joined = if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    };
    validate_remote_path(&joined)?;
    Ok(joined)
}

pub fn parent_remote_path(path: &str) -> Result<String, String> {
    validate_remote_path(path)?;
    if path == "/" {
        return Ok("/".into());
    }
    let trimmed = path.trim_end_matches('/');
    let Some(index) = trimmed.rfind('/') else {
        return Ok(".".into());
    };
    Ok(if index == 0 {
        "/".into()
    } else {
        trimmed[..index].to_string()
    })
}

pub fn validate_local_transfer_path(path: &Path) -> Result<(), String> {
    let value = path
        .to_str()
        .ok_or_else(|| "首个版本仅支持 UTF-8 本地路径".to_string())?;
    validate_absolute_local_path("本地文件路径", value)
}

fn validate_initial_remote_path(path: &str) -> Result<(), String> {
    validate_remote_path(path)?;
    if path != "." && !path.starts_with('/') {
        return Err("初始远程目录必须是绝对路径，或使用 . 表示默认目录".into());
    }
    Ok(())
}

fn validate_absolute_local_path(label: &str, value: &str) -> Result<(), String> {
    validate_required_single_line(label, value, MAX_SSH_PATH_BYTES)?;
    if !Path::new(value).is_absolute() {
        return Err(format!("{label}必须是绝对路径"));
    }
    Ok(())
}

fn validate_color(value: &str) -> Result<(), String> {
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err("颜色标签必须是 #RRGGBB 格式".into())
    }
}

fn validate_optional_single_line(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_single_line(label, value, max_bytes)?;
    }
    Ok(())
}

fn validate_required_single_line(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label}不能为空"));
    }
    validate_single_line(label, value, max_bytes)
}

fn validate_single_line(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    validate_protocol_text(label, value, max_bytes)?;
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

fn validate_protocol_text(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!(
            "{label}过长：{} bytes，最多 {max_bytes} bytes",
            value.len()
        ));
    }
    if value.contains('\0') {
        return Err(format!("{label}不能包含 NUL 字符"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_rejects_option_injection_and_unsupported_key() {
        let mut profile = SshProfile::new("server", "-oProxyCommand=bad");
        assert!(profile.validate().is_err());

        profile.host = "server.example".into();
        profile.auth_mode = SshAuthMode::KeyFile;
        profile.key_path = Some(if cfg!(windows) {
            r"C:\\keys\\server.ppk".into()
        } else {
            "/keys/server.ppk".into()
        });
        assert!(matches!(
            profile.validate(),
            Err(error) if error.contains(".ppk")
        ));
    }

    #[test]
    fn profile_requires_absolute_paths() {
        let mut profile = SshProfile::new("server", "example.com");
        profile.auth_mode = SshAuthMode::KeyFile;
        profile.key_path = Some("relative-key".into());
        assert!(matches!(
            profile.validate(),
            Err(error) if error.contains("绝对路径")
        ));

        profile.auth_mode = SshAuthMode::System;
        profile.key_path = None;
        profile.ssh_path = Some("ssh".into());
        assert!(matches!(
            profile.validate(),
            Err(error) if error.contains("绝对路径")
        ));
    }

    #[test]
    fn remote_paths_do_not_escape_through_names() {
        assert_eq!(
            join_remote_path("/home/user", "file.txt").as_deref(),
            Ok("/home/user/file.txt")
        );
        assert_eq!(
            parent_remote_path("/home/user/file.txt").as_deref(),
            Ok("/home/user")
        );
        assert_eq!(parent_remote_path("/").as_deref(), Ok("/"));
        assert!(join_remote_path("/home/user", "../secret").is_err());
        assert!(join_remote_path("/home/user", "dir/file").is_err());
    }

    #[test]
    fn transfer_status_is_terminal_once_finished() {
        let mut task = TransferTask::new(
            SshProfileId::new(),
            TransferDirection::Download,
            "/tmp/local",
            "/remote/file",
        );
        task.mark_running();
        task.update_progress(5, 10);
        task.finish(Ok(()), false);
        task.finish(Err("late error".into()), false);

        assert_eq!(task.status, TransferStatus::Completed);
        assert_eq!(task.transferred_bytes, 5);
        assert!(task.error.is_none());
    }
}
