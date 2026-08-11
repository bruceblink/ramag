//! 云对象存储账号、挂载点、对象与传输实体。

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

mod transfer;
mod validation;
mod workspace;
pub use transfer::{
    ObjectDownloadRequest, ObjectProgressFn, ObjectTransferProgress, ObjectUploadRequest,
};
pub use validation::{
    is_opendal_safe_key, is_opendal_safe_list_prefix, is_opendal_safe_prefix, validate_bucket_name,
    validate_bucket_name_for_provider, validate_object_key, validate_object_name_prefix,
    validate_prefix, validate_region, validate_root_prefix,
};
pub use workspace::{
    ObjectStorageFavorite, ObjectStorageSessionPreference, ObjectStorageWorkspacePreference,
    ObjectStorageWorkspaceState,
};

pub const OBJECT_STORAGE_ACCOUNT_SCHEMA_VERSION: u16 = 1;
pub const MAX_OBJECT_STORAGE_ACCOUNTS: usize = 64;
pub const MAX_OBJECT_STORAGE_ACCOUNT_NAME_BYTES: usize = 128;
pub const MAX_OBJECT_STORAGE_ACCESS_KEY_ID_BYTES: usize = 256;
pub const MAX_OBJECT_STORAGE_ACCESS_KEY_SECRET_BYTES: usize = 512;
pub const MAX_MANUAL_BUCKETS_PER_ACCOUNT: usize = 128;
pub const MAX_OBJECT_STORAGE_BUCKET_NAME_BYTES: usize = 255;
pub const MAX_OBJECT_STORAGE_REGION_BYTES: usize = 128;
pub const MAX_OBJECT_STORAGE_ENDPOINT_BYTES: usize = 2 * 1024;
pub const MAX_OBJECT_STORAGE_KEY_BYTES: usize = 4 * 1024;
pub const MAX_OBJECT_STORAGE_PAGE_ENTRIES: usize = 500;
pub const MAX_OBJECT_STORAGE_WORKSPACE_ENTRIES: usize = 20_000;
pub const MAX_OBJECT_STORAGE_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_OBJECT_STORAGE_SESSION_BYTES: usize = 16 * 1024;
pub const MAX_OBJECT_STORAGE_CONCURRENT_TRANSFERS: usize = 3;
pub const MAX_OBJECT_STORAGE_QUEUED_TRANSFERS: usize = 64;
pub const MAX_OBJECT_STORAGE_TRANSFER_HISTORY: usize = 100;
pub const MAX_OBJECT_STORAGE_TEXT_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
pub const OBJECT_STORAGE_TRANSFER_BUFFER_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectStorageAccountId(pub Uuid);

impl ObjectStorageAccountId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ObjectStorageAccountId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ObjectStorageAccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectStorageMountId(pub Uuid);

impl ObjectStorageMountId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ObjectStorageMountId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ObjectStorageMountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    TencentCos,
    AliyunOss,
}

impl CloudProvider {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::TencentCos => "腾讯云 COS",
            Self::AliyunOss => "阿里云 OSS",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualBucket {
    pub id: ObjectStorageMountId,
    pub name: String,
    pub region: String,
    #[serde(default)]
    pub root_prefix: Option<String>,
}

impl ManualBucket {
    pub fn new(name: impl Into<String>, region: impl Into<String>) -> Self {
        Self {
            id: ObjectStorageMountId::new(),
            name: name.into(),
            region: region.into(),
            root_prefix: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_bucket_name(&self.name)?;
        validate_region(&self.region)?;
        if let Some(prefix) = &self.root_prefix {
            validate_root_prefix(prefix)?;
        }
        Ok(())
    }

    pub fn validate_for_provider(&self, provider: CloudProvider) -> Result<(), String> {
        self.validate()?;
        validate_bucket_name_for_provider(provider, &self.name)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectStorageAccount {
    pub schema_version: u16,
    pub id: ObjectStorageAccountId,
    pub revision: u64,
    pub name: String,
    pub provider: CloudProvider,
    pub access_key_id: SecretString,
    pub access_key_secret: SecretString,
    pub read_only: bool,
    pub manual_buckets: Vec<ManualBucket>,
}

impl fmt::Debug for ObjectStorageAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStorageAccount")
            .field("schema_version", &self.schema_version)
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("name", &self.name)
            .field("provider", &self.provider)
            .field("access_key_id", &"[REDACTED]")
            .field("access_key_secret", &"[REDACTED]")
            .field("read_only", &self.read_only)
            .field("manual_buckets", &self.manual_buckets)
            .finish()
    }
}

impl ObjectStorageAccount {
    pub fn new(name: impl Into<String>, provider: CloudProvider) -> Self {
        Self {
            schema_version: OBJECT_STORAGE_ACCOUNT_SCHEMA_VERSION,
            id: ObjectStorageAccountId::new(),
            revision: 1,
            name: name.into(),
            provider,
            access_key_id: SecretString::new(""),
            access_key_secret: SecretString::new(""),
            read_only: false,
            manual_buckets: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != OBJECT_STORAGE_ACCOUNT_SCHEMA_VERSION {
            return Err(format!("不支持的云存储账号版本：{}", self.schema_version));
        }
        if self.revision == 0 {
            return Err("云存储账号 revision 不能为 0".into());
        }
        validate_required_single_line(
            "账号名称",
            &self.name,
            MAX_OBJECT_STORAGE_ACCOUNT_NAME_BYTES,
        )?;
        validate_credential(
            "AccessKey ID / SecretId",
            &self.access_key_id,
            MAX_OBJECT_STORAGE_ACCESS_KEY_ID_BYTES,
        )?;
        validate_credential(
            "AccessKey Secret / SecretKey",
            &self.access_key_secret,
            MAX_OBJECT_STORAGE_ACCESS_KEY_SECRET_BYTES,
        )?;
        if self.manual_buckets.is_empty() {
            return Err("请至少添加一个 Bucket".into());
        }
        if self.manual_buckets.len() > MAX_MANUAL_BUCKETS_PER_ACCOUNT {
            return Err(format!(
                "Bucket 挂载数量超过 {MAX_MANUAL_BUCKETS_PER_ACCOUNT} 个上限"
            ));
        }
        for bucket in &self.manual_buckets {
            bucket.validate_for_provider(self.provider)?;
        }
        let mut ids = HashSet::new();
        let mut mounts = HashSet::new();
        for bucket in &self.manual_buckets {
            if !ids.insert(bucket.id.clone()) {
                return Err("Bucket 挂载点 ID 重复".into());
            }
            let identity = (
                bucket.name.as_str(),
                bucket.region.as_str(),
                bucket.root_prefix.as_deref().unwrap_or(""),
            );
            if !mounts.insert(identity) {
                return Err("同一 Bucket、地域和 Root Prefix 不能重复挂载".into());
            }
        }
        Ok(())
    }

    pub fn next_revision(&self) -> u64 {
        self.revision.saturating_add(1)
    }

    pub fn snapshot(&self) -> ObjectStorageAccountSnapshot {
        ObjectStorageAccountSnapshot {
            id: self.id.clone(),
            revision: self.revision,
            provider: self.provider,
            access_key_id: self.access_key_id.clone(),
            access_key_secret: self.access_key_secret.clone(),
            read_only: self.read_only,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ObjectStorageAccountSnapshot {
    pub id: ObjectStorageAccountId,
    pub revision: u64,
    pub provider: CloudProvider,
    pub access_key_id: SecretString,
    pub access_key_secret: SecretString,
    pub read_only: bool,
}

impl fmt::Debug for ObjectStorageAccountSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStorageAccountSnapshot")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("provider", &self.provider)
            .field("credentials", &"[REDACTED]")
            .field("read_only", &self.read_only)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HttpsEndpoint(String);

impl HttpsEndpoint {
    pub fn parse_official(provider: CloudProvider, value: &str) -> Result<Self, String> {
        let normalized = value.trim_end_matches('/');
        if normalized.len() > MAX_OBJECT_STORAGE_ENDPOINT_BYTES {
            return Err("Endpoint 超过 2 KiB 上限".into());
        }
        let host = normalized
            .strip_prefix("https://")
            .ok_or_else(|| "Endpoint 必须使用 HTTPS".to_string())?;
        if host.is_empty()
            || host.contains(['/', '?', '#', '@', ':'])
            || !host.is_ascii()
            || host != host.to_ascii_lowercase()
        {
            return Err("Endpoint 必须是无路径、端口和用户信息的官方 HTTPS 主机".into());
        }
        let valid = match provider {
            CloudProvider::TencentCos => host
                .strip_prefix("cos.")
                .and_then(|value| value.strip_suffix(".myqcloud.com"))
                .is_some_and(|region| !region.contains('.') && validate_region(region).is_ok()),
            CloudProvider::AliyunOss => host
                .strip_prefix("oss-")
                .and_then(|value| value.strip_suffix(".aliyuncs.com"))
                .is_some_and(|region| !region.contains('.') && validate_region(region).is_ok()),
        };
        if !valid {
            return Err(format!(
                "Endpoint 不是 {} 官方主机",
                provider.display_name()
            ));
        }
        Ok(Self(normalized.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStorageMount {
    pub id: ObjectStorageMountId,
    pub account_id: ObjectStorageAccountId,
    pub bucket: String,
    pub region: String,
    pub endpoint: HttpsEndpoint,
    pub root_prefix: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub storage_class: Option<String>,
}

impl ObjectStorageMount {
    pub fn operator_identity(&self, revision: u64) -> String {
        format!(
            "{}:{revision}:{}:{}:{}:{}",
            self.account_id,
            self.bucket,
            self.region,
            self.endpoint.as_str(),
            self.root_prefix.as_deref().unwrap_or("")
        )
    }

    pub fn validate_for_provider(&self, provider: CloudProvider) -> Result<(), String> {
        validate_bucket_name_for_provider(provider, &self.bucket)?;
        validate_region(&self.region)?;
        HttpsEndpoint::parse_official(provider, self.endpoint.as_str())?;
        if let Some(root_prefix) = &self.root_prefix {
            validate_root_prefix(root_prefix)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectEntryKind {
    Prefix,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectEntry {
    pub key: String,
    pub display_name: String,
    pub kind: ObjectEntryKind,
    pub operable: bool,
    pub size: Option<u64>,
    pub last_modified: Option<DateTime<Utc>>,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub storage_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectListCursor(String);

impl ObjectListCursor {
    pub fn new() -> Self {
        Self(Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ObjectListCursor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPage {
    pub entries: Vec<ObjectEntry>,
    pub next_cursor: Option<ObjectListCursor>,
    pub capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectListQuery {
    directory_prefix: String,
    name_prefix: String,
    list_prefix: String,
}

impl ObjectListQuery {
    pub fn new(directory_prefix: &str, name_prefix: &str) -> Result<Self, String> {
        if !is_opendal_safe_prefix(directory_prefix) {
            return Err("当前目录前缀无法由 OpenDAL 安全表示".into());
        }
        validate_object_name_prefix(name_prefix)?;
        let list_prefix = format!("{directory_prefix}{name_prefix}");
        if !is_opendal_safe_list_prefix(&list_prefix) {
            return Err("对象名称前缀无法由 OpenDAL 安全表示".into());
        }
        Ok(Self {
            directory_prefix: directory_prefix.to_string(),
            name_prefix: name_prefix.to_string(),
            list_prefix,
        })
    }

    pub fn directory_prefix(&self) -> &str {
        &self.directory_prefix
    }

    pub fn name_prefix(&self) -> &str {
        &self.name_prefix
    }

    pub fn list_prefix(&self) -> &str {
        &self.list_prefix
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub key: String,
    pub size: u64,
    pub last_modified: Option<DateTime<Utc>>,
    pub etag: Option<String>,
    pub version: Option<String>,
    pub content_type: Option<String>,
    pub user_metadata: Vec<(String, String)>,
    pub storage_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTextPreview {
    pub content: String,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectCapabilities {
    pub stat: bool,
    pub read: bool,
    pub write: bool,
    pub delete: bool,
    pub list: bool,
    pub atomic_create: bool,
}

fn validate_credential(label: &str, value: &SecretString, max: usize) -> Result<(), String> {
    let value = value.expose_secret();
    if value.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    validate_protocol_text(label, value, max)?;
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

fn validate_required_single_line(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label}不能为空"));
    }
    validate_protocol_text(label, value, max)?;
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符"));
    }
    Ok(())
}

pub(super) fn validate_protocol_text(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        return Err(format!(
            "{label}过长：{} bytes，最多 {max} bytes",
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
