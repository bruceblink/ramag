//! SSH 配置、远程文件与传输任务实体。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::jumpserver::JumpServerRdpSession;
use super::ssh_diagnostic::RemotePlatformPreference;
use super::ssh_remote_path::{RemotePath, infer_sftp_namespace};

pub const MAX_SSH_PROFILES: usize = 1024;
pub const MAX_SSH_PROFILE_NAME_BYTES: usize = 256;
pub const MAX_SSH_HOST_BYTES: usize = 1024;
pub const MAX_SSH_USERNAME_BYTES: usize = 1024;
/// AskPass 只读取单行凭据，并使用 1024 字节缓冲区（含结尾 NUL）。
pub const MAX_SSH_PASSWORD_BYTES: usize = 1023;
pub const MAX_SSH_ENVIRONMENT_BYTES: usize = 64;
pub const MAX_SSH_PATH_BYTES: usize = 32 * 1024;
pub const MAX_REMOTE_DIRECTORY_ENTRIES: usize = 100_000;
pub const MAX_REMOTE_DIRECTORY_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REMOTE_FILE_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_REMOTE_DELETE_DEPTH: usize = 64;
pub const MAX_REMOTE_DELETE_ENTRIES: usize = 100_000;
pub const MAX_REMOTE_DELETE_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REMOTE_ARCHIVE_DEPTH: usize = 64;
pub const MAX_REMOTE_ARCHIVE_ENTRIES: usize = 100_000;
pub const MAX_REMOTE_ARCHIVE_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TRANSFER_HISTORY: usize = 100;
pub const MAX_QUEUED_TRANSFERS: usize = 64;
pub const MAX_CONCURRENT_TRANSFERS: usize = 3;
pub const MAX_PRODUCTION_DIRECTORY_ENTRIES: usize = 5_000;
pub const MAX_PRODUCTION_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_CONCURRENT_PRODUCTION_DOWNLOADS: usize = 1;
pub const MAX_PRODUCTION_DOWNLOAD_SECONDS: u64 = 300;
pub const MAX_SSH_WORKSPACES: usize = 16;
pub const MAX_SSH_TERMINALS_PER_WORKSPACE: usize = 8;
pub const MAX_SSH_FAVORITE_PATHS_PER_PROFILE: usize = 16;
pub const TRANSFER_BUFFER_BYTES: usize = 64 * 1024;

/// SSH 模块级通用配置。
/// 该配置不属于单个连接，修改后对所有 SSH/SFTP 连接生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SshModuleSettings {
    /// Windows 标准 SFTP 无法列出目录或盘符时，改用远端 sftp-server.exe。
    #[serde(default)]
    pub windows_sftp_compatibility: bool,
}

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
    Password,
    KeyFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshProfileOrigin {
    #[default]
    Manual,
    JumpServer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfile {
    pub id: SshProfileId,
    pub name: String,
    /// 连接来源用于保留导入语义；Windows SFTP 兼容通道由独立配置决定。
    #[serde(default)]
    pub origin: SshProfileOrigin,
    /// 环境仅用于列表徽章展示，不影响连接行为。
    #[serde(default)]
    pub environment: Option<String>,
    /// 生产模式显示风险提示，并禁止 SFTP 远程写操作。
    #[serde(default)]
    pub production: bool,
    /// 用户对远端平台的偏好；真实平台仍由当前会话探测确认。
    #[serde(default)]
    pub remote_platform: RemotePlatformPreference,
    /// 运行时 Windows SFTP 通道选择；真实配置来自模块设置，不写入单条连接。
    #[serde(skip)]
    pub windows_sftp_compatibility: bool,
    /// JumpServer 导入时的 RDP Web 协议能力快照；`None` 表示手动或旧记录未探测。
    #[serde(default)]
    pub rdp_web_enabled: Option<bool>,
    /// 可直接复用的 JumpServer RDP 目标；不包含密码或 API Token。
    #[serde(default)]
    pub jumpserver_rdp_session: Option<JumpServerRdpSession>,
    /// 主机名、IP 或 `~/.ssh/config` 别名。
    pub host: String,
    /// 留空时由 `~/.ssh/config` 决定，未配置则由 OpenSSH 使用 22。
    #[serde(default)]
    pub port: Option<u16>,
    /// 留空时交给 OpenSSH 配置解析。
    pub username: String,
    pub auth_mode: SshAuthMode,
    /// 仅密码认证使用；整条配置由存储层加密后落盘。
    #[serde(default)]
    pub password: String,
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
            origin: SshProfileOrigin::Manual,
            environment: None,
            production: false,
            remote_platform: RemotePlatformPreference::Auto,
            windows_sftp_compatibility: false,
            rdp_web_enabled: None,
            jumpserver_rdp_session: None,
            host: host.into(),
            port: None,
            username: String::new(),
            auth_mode: SshAuthMode::System,
            password: String::new(),
            key_path: None,
            initial_directory: None,
            ssh_path: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_required_single_line("连接名称", &self.name, MAX_SSH_PROFILE_NAME_BYTES)?;
        validate_optional_single_line(
            "环境标签",
            self.environment.as_deref(),
            MAX_SSH_ENVIRONMENT_BYTES,
        )?;
        validate_required_single_line("主机或 SSH 别名", &self.host, MAX_SSH_HOST_BYTES)?;
        if self.host.starts_with('-') {
            return Err("主机或 SSH 别名不能以 '-' 开头".into());
        }
        if self.host.chars().any(char::is_whitespace) {
            return Err("主机或 SSH 别名不能包含空白字符".into());
        }
        if self.port == Some(0) {
            return Err("SSH 端口必须在 1–65535 之间".into());
        }
        validate_optional_single_line("用户名", Some(&self.username), MAX_SSH_USERNAME_BYTES)?;
        if self.username.chars().any(char::is_whitespace) {
            return Err("用户名不能包含空白字符".into());
        }

        match self.auth_mode {
            SshAuthMode::System => {
                if self.key_path.is_some() {
                    return Err("系统 SSH 配置 / Agent 认证不能同时指定密钥文件".into());
                }
                if !self.password.is_empty() {
                    return Err("系统 SSH 配置 / Agent 认证不能同时保存密码".into());
                }
            }
            SshAuthMode::Password => {
                if self.key_path.is_some() {
                    return Err("密码认证不能同时指定密钥文件".into());
                }
                validate_required_password(&self.password)?;
            }
            SshAuthMode::KeyFile => {
                if !self.password.is_empty() {
                    return Err("密钥认证不能同时保存密码".into());
                }
                let key_path = self
                    .key_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| "密钥文件认证必须选择密钥路径".to_string())?;
                validate_absolute_local_path("密钥路径", key_path)?;
                if key_path.to_ascii_lowercase().ends_with(".ppk") {
                    return Err("暂不支持 PuTTY .ppk 密钥，请转换为 OpenSSH 格式".into());
                }
            }
        }

        if let Some(path) = self.initial_directory.as_deref() {
            validate_initial_remote_path(path)?;
        }
        if let Some(path) = self.ssh_path.as_deref() {
            validate_absolute_local_path("OpenSSH 可执行文件路径", path)?;
        }
        if self.windows_sftp_compatibility
            && self.remote_platform == RemotePlatformPreference::Linux
        {
            return Err("Windows SFTP 兼容模式不能用于明确的 Linux 远端".into());
        }
        if let Some(session) = self.jumpserver_rdp_session.as_ref() {
            session.validate()?;
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
    /// 应用层终端策略代次；启动 PTY 前必须再次确认仍为当前代次。
    pub authorization_generation: u64,
    pub program: String,
    pub args: Vec<String>,
    /// 不含明文密码；一次性 AskPass 令牌仍只应传给目标进程。
    pub env: HashMap<String, String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFilePreview {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
}

/// 远程文件分段读取位置；每次返回的数据仍受预览字节上限约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteFileChunkPosition {
    From(u64),
    Before(u64),
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFileChunk {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub total_bytes: u64,
}

impl RemoteFileChunk {
    pub fn end_offset(&self) -> u64 {
        self.offset.saturating_add(self.bytes.len() as u64)
    }
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
    DownloadArchive,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshPathFavorites {
    pub profile_id: SshProfileId,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SshWorkspacePreference {
    pub workspaces: Vec<SshWorkspaceState>,
    pub active_profile_id: Option<SshProfileId>,
    #[serde(default)]
    pub path_favorites: Vec<SshPathFavorites>,
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
    let namespace = infer_sftp_namespace(parent);
    let parent = RemotePath::parse_with_namespace(parent, namespace)?;
    parent.join_child(name).map(|path| path.to_string())
}

pub fn parent_remote_path(path: &str) -> Result<String, String> {
    let namespace = infer_sftp_namespace(path);
    let path = RemotePath::parse_with_namespace(path, namespace)?;
    Ok(path.parent().to_string())
}

pub fn validate_local_transfer_path(path: &Path) -> Result<(), String> {
    let value = path
        .to_str()
        .ok_or_else(|| "仅支持 UTF-8 本地路径".to_string())?;
    validate_absolute_local_path("本地文件路径", value)
}

fn validate_initial_remote_path(path: &str) -> Result<(), String> {
    validate_remote_path(path)?;
    if path == "." {
        return Ok(());
    }
    RemotePath::parse_server_canonical(path)
        .map(|_| ())
        .map_err(|_| "初始远程目录必须是 /path、C:/path，或使用 . 表示默认目录".into())
}

fn validate_absolute_local_path(label: &str, value: &str) -> Result<(), String> {
    validate_required_single_line(label, value, MAX_SSH_PATH_BYTES)?;
    if !Path::new(value).is_absolute() {
        return Err(format!("{label}必须是绝对路径"));
    }
    Ok(())
}

fn validate_required_password(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("密码不能为空".into());
    }
    validate_protocol_text("密码", value, MAX_SSH_PASSWORD_BYTES)?;
    if value.chars().any(char::is_control) {
        return Err("密码不能包含换行或控制字符".into());
    }
    Ok(())
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
mod tests;
