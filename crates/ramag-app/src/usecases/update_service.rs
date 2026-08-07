//! 应用更新用例：版本比较、检查节流与平台安装包选择。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::lock::Mutex;
use parking_lot::RwLock;
use ramag_domain::entities::{ReleaseAsset, ReleaseInfo, UpdateCancellation, UpdateProgressFn};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{Storage, UpdateDriver};
use semver::Version;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

pub const UPDATE_CHECK_PREF_KEY: &str = "update_last_check_at_v1";
pub const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const UPDATE_RESULT_PREF_KEY: &str = "update_last_result_v1";
const MAX_CACHED_UPDATE_NOTICE_BYTES: usize = 256;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedUpdateNotice {
    current_version: String,
    latest_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlatform {
    MacosArm64,
    MacosX86_64,
    WindowsX86_64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub release: ReleaseInfo,
    pub asset: Option<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckResult {
    UpToDate {
        current_version: String,
        latest_version: String,
    },
    Available(AvailableUpdate),
    UnsupportedPlatform(AvailableUpdate),
}

pub struct UpdateService {
    driver: Arc<dyn UpdateDriver>,
    storage: Arc<dyn Storage>,
    current_version: String,
    check_lock: Mutex<()>,
    last_result: RwLock<Option<UpdateCheckResult>>,
}

impl UpdateService {
    pub fn new(
        driver: Arc<dyn UpdateDriver>,
        storage: Arc<dyn Storage>,
        current_version: impl Into<String>,
    ) -> Self {
        Self {
            driver,
            storage,
            current_version: current_version.into(),
            check_lock: Mutex::new(()),
            last_result: RwLock::new(None),
        }
    }

    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    pub fn last_result(&self) -> Option<UpdateCheckResult> {
        self.last_result.read().clone()
    }

    pub async fn check(&self, force: bool) -> Result<UpdateCheckResult> {
        let _guard = self.check_lock.lock().await;
        info!(force, current_version = %self.current_version, "update check started");
        if !force
            && !self.should_check().await
            && let Some(result) = self.load_cached_result().await
        {
            *self.last_result.write() = Some(result.clone());
            info!(
                result = update_result_kind(&result),
                "cached update check result restored"
            );
            return Ok(result);
        }
        self.mark_check_attempt().await;

        let release = match self.driver.latest_stable_release().await {
            Ok(release) => release,
            Err(error) => {
                warn!(error = %error, force, "update check failed");
                return Err(error);
            }
        };
        let result = match evaluate_release(&self.current_version, release, current_platform()) {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, force, "update metadata validation failed");
                return Err(error);
            }
        };
        self.persist_result(&result).await;
        *self.last_result.write() = Some(result.clone());
        info!(
            result = update_result_kind(&result),
            "update check completed"
        );
        Ok(result)
    }

    pub async fn download(
        &self,
        update: &AvailableUpdate,
        progress: UpdateProgressFn,
        cancellation: UpdateCancellation,
    ) -> Result<std::path::PathBuf> {
        let asset = update
            .asset
            .as_ref()
            .ok_or_else(|| DomainError::NotImplemented("当前平台没有可下载的安装包".into()))?;
        let expected_name = current_platform()
            .map(|platform| asset_name_for(platform, &update.release.version))
            .ok_or_else(|| DomainError::NotImplemented("当前平台没有可下载的安装包".into()))?;
        if asset.name != expected_name {
            return Err(DomainError::InvalidConfig(format!(
                "更新资产与当前平台不匹配：{}",
                asset.name
            )));
        }
        let current = Version::parse(&self.current_version)
            .map_err(|error| DomainError::InvalidConfig(format!("当前应用版本无效：{error}")))?;
        let latest = Version::parse(&update.release.version)
            .map_err(|error| DomainError::InvalidConfig(format!("更新版本无效：{error}")))?;
        if latest <= current {
            return Err(DomainError::Other("更新版本不高于当前版本".into()));
        }
        info!(version = %update.release.version, asset = %asset.name, expected_bytes = asset.size, "update download started");
        let result = self
            .driver
            .download_asset(&update.release, asset, progress, cancellation)
            .await;
        match &result {
            Ok(path) => {
                info!(version = %update.release.version, asset = %asset.name, path = %path.display(), "update download completed")
            }
            Err(download_error) => {
                error!(error = %download_error, version = %update.release.version, asset = %asset.name, "update download failed")
            }
        }
        result
    }

    pub fn reveal_download(&self, path: &std::path::Path) -> Result<()> {
        self.driver.reveal_download(path)
    }

    async fn should_check(&self) -> bool {
        let value = match self.storage.get_preference(UPDATE_CHECK_PREF_KEY).await {
            Ok(value) => value,
            Err(error) => {
                warn!(error = %error, "load update check timestamp failed");
                return true;
            }
        };
        let Some(value) = value else { return true };
        let Ok(last) = value.parse::<u64>() else {
            return true;
        };
        let now = unix_timestamp();
        now < last || now.saturating_sub(last) >= AUTO_CHECK_INTERVAL.as_secs()
    }

    async fn mark_check_attempt(&self) {
        if let Err(error) = self
            .storage
            .set_preference(UPDATE_CHECK_PREF_KEY, &unix_timestamp().to_string())
            .await
        {
            warn!(error = %error, "persist update check timestamp failed");
        }
    }

    async fn load_cached_result(&self) -> Option<UpdateCheckResult> {
        let value = match self.storage.get_preference(UPDATE_RESULT_PREF_KEY).await {
            Ok(value) => value,
            Err(error) => {
                warn!(error = %error, "load cached update result failed");
                return None;
            }
        }?;
        match parse_cached_result(&self.current_version, &value) {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, "parse cached update result failed");
                None
            }
        }
    }

    async fn persist_result(&self, result: &UpdateCheckResult) {
        let notice = cached_notice(&self.current_version, result);
        let value = match serde_json::to_string(&notice) {
            Ok(value) => value,
            Err(error) => {
                warn!(error = %error, "serialize update result failed");
                return;
            }
        };
        if let Err(error) = self
            .storage
            .set_preference(UPDATE_RESULT_PREF_KEY, &value)
            .await
        {
            warn!(error = %error, "persist update result failed");
        }
    }
}

fn update_result_kind(result: &UpdateCheckResult) -> &'static str {
    match result {
        UpdateCheckResult::UpToDate { .. } => "up_to_date",
        UpdateCheckResult::Available(_) => "available",
        UpdateCheckResult::UnsupportedPlatform(_) => "unsupported_platform",
    }
}

fn cached_notice(current_version: &str, result: &UpdateCheckResult) -> CachedUpdateNotice {
    let latest_version = match result {
        UpdateCheckResult::UpToDate { latest_version, .. } => latest_version.clone(),
        UpdateCheckResult::Available(update) | UpdateCheckResult::UnsupportedPlatform(update) => {
            update.release.version.clone()
        }
    };
    CachedUpdateNotice {
        current_version: current_version.into(),
        latest_version,
    }
}

fn parse_cached_result(current_version: &str, value: &str) -> Result<Option<UpdateCheckResult>> {
    if value.len() > MAX_CACHED_UPDATE_NOTICE_BYTES {
        return Err(DomainError::Storage("缓存更新结果过大".into()));
    }
    let cached: CachedUpdateNotice = serde_json::from_str(value)
        .map_err(|error| DomainError::Storage(format!("缓存更新结果无效：{error}")))?;
    if cached.current_version != current_version {
        return Ok(None);
    }
    let current = Version::parse(current_version)
        .map_err(|error| DomainError::InvalidConfig(format!("当前应用版本无效：{error}")))?;
    let latest = Version::parse(&cached.latest_version)
        .map_err(|error| DomainError::Storage(format!("缓存更新版本无效：{error}")))?;
    if current.to_string() != current_version
        || latest.to_string() != cached.latest_version
        || !latest.pre.is_empty()
    {
        return Err(DomainError::Storage("缓存更新版本不是规范稳定版本".into()));
    }
    if latest <= current {
        return Ok(Some(UpdateCheckResult::UpToDate {
            current_version: current_version.into(),
            latest_version: cached.latest_version,
        }));
    }
    let tag_name = format!("v{}", cached.latest_version);
    Ok(Some(UpdateCheckResult::UnsupportedPlatform(
        AvailableUpdate {
            release: ReleaseInfo {
                version: cached.latest_version,
                release_url: format!("https://github.com/tools-rs/ramag/releases/tag/{tag_name}"),
                tag_name,
                notes: String::new(),
                published_at: None,
                assets: Vec::new(),
            },
            asset: None,
        },
    )))
}

pub fn asset_name_for(platform: UpdatePlatform, version: &str) -> String {
    match platform {
        UpdatePlatform::MacosArm64 => format!("Ramag-{version}-macos-arm64.dmg"),
        UpdatePlatform::MacosX86_64 => format!("Ramag-{version}-macos-x86_64.dmg"),
        UpdatePlatform::WindowsX86_64 => format!("Ramag-{version}-windows-x64-setup.exe"),
    }
}

fn evaluate_release(
    current_version: &str,
    release: ReleaseInfo,
    platform: Option<UpdatePlatform>,
) -> Result<UpdateCheckResult> {
    let current = Version::parse(current_version).map_err(|error| {
        DomainError::InvalidConfig(format!("当前应用版本无效 {current_version}：{error}"))
    })?;
    let latest = Version::parse(&release.version).map_err(|error| {
        DomainError::Other(format!(
            "GitHub Release 版本无效 {}：{error}",
            release.version
        ))
    })?;
    if !latest.pre.is_empty() {
        return Err(DomainError::Other(format!(
            "稳定更新通道返回了预发布版本：{latest}"
        )));
    }
    if latest <= current {
        return Ok(UpdateCheckResult::UpToDate {
            current_version: current_version.to_string(),
            latest_version: release.version,
        });
    }
    let asset = platform
        .map(|platform| asset_name_for(platform, &release.version))
        .and_then(|name| release.assets.iter().find(|asset| asset.name == name))
        .cloned();
    let update = AvailableUpdate { release, asset };
    if update.asset.is_some() {
        Ok(UpdateCheckResult::Available(update))
    } else {
        Ok(UpdateCheckResult::UnsupportedPlatform(update))
    }
}

pub fn current_platform() -> Option<UpdatePlatform> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some(UpdatePlatform::MacosArm64);
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some(UpdatePlatform::MacosX86_64);
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some(UpdatePlatform::WindowsX86_64);
    }
    #[allow(unreachable_code)]
    None
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use ramag_domain::entities::{ReleaseAsset, ReleaseInfo};

    use super::{
        UpdateCheckResult, UpdatePlatform, asset_name_for, cached_notice, evaluate_release,
        parse_cached_result,
    };

    #[test]
    fn asset_names_are_exact_and_platform_specific() {
        assert_eq!(
            asset_name_for(UpdatePlatform::MacosArm64, "1.2.3"),
            "Ramag-1.2.3-macos-arm64.dmg"
        );
        assert_eq!(
            asset_name_for(UpdatePlatform::MacosX86_64, "1.2.3"),
            "Ramag-1.2.3-macos-x86_64.dmg"
        );
        assert_eq!(
            asset_name_for(UpdatePlatform::WindowsX86_64, "1.2.3"),
            "Ramag-1.2.3-windows-x64-setup.exe"
        );
    }

    fn release(version: &str, with_asset: bool) -> ReleaseInfo {
        let name = asset_name_for(UpdatePlatform::MacosArm64, version);
        ReleaseInfo {
            version: version.into(),
            tag_name: format!("v{version}"),
            release_url: format!("https://github.com/tools-rs/ramag/releases/tag/v{version}"),
            notes: String::new(),
            published_at: None,
            assets: with_asset
                .then(|| ReleaseAsset {
                    name,
                    download_url: "https://github.com/tools-rs/ramag/releases/download/file".into(),
                    size: 1,
                    sha256: None,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn newer_version_requires_exact_platform_asset() {
        let available = evaluate_release(
            "1.0.0",
            release("1.1.0", true),
            Some(UpdatePlatform::MacosArm64),
        )
        .expect("newer release should evaluate");
        assert!(matches!(available, UpdateCheckResult::Available(_)));

        let unsupported = evaluate_release(
            "1.0.0",
            release("1.1.0", false),
            Some(UpdatePlatform::MacosArm64),
        )
        .expect("newer release should evaluate");
        assert!(matches!(
            unsupported,
            UpdateCheckResult::UnsupportedPlatform(_)
        ));
    }

    #[test]
    fn same_older_and_prerelease_versions_are_handled_conservatively() {
        assert!(matches!(
            evaluate_release(
                "1.0.0",
                release("1.0.0", true),
                Some(UpdatePlatform::MacosArm64)
            ),
            Ok(UpdateCheckResult::UpToDate { .. })
        ));
        assert!(matches!(
            evaluate_release(
                "1.1.0",
                release("1.0.0", true),
                Some(UpdatePlatform::MacosArm64)
            ),
            Ok(UpdateCheckResult::UpToDate { .. })
        ));
        assert!(
            evaluate_release(
                "1.0.0",
                release("1.1.0-beta.1", true),
                Some(UpdatePlatform::MacosArm64)
            )
            .is_err()
        );
    }

    #[test]
    fn cached_update_notice_restores_after_restart_without_remote_metadata() {
        let available = evaluate_release(
            "1.0.0",
            release("1.1.0", true),
            Some(UpdatePlatform::MacosArm64),
        )
        .expect("newer release should evaluate");
        let notice = cached_notice("1.0.0", &available);
        let value = serde_json::to_string(&notice).expect("notice should serialize");

        let restored = parse_cached_result("1.0.0", &value)
            .expect("cached notice should parse")
            .expect("same app version should restore");
        let UpdateCheckResult::UnsupportedPlatform(update) = restored else {
            panic!("cached update should restore as a safe update notice");
        };
        assert_eq!(update.release.version, "1.1.0");
        assert_eq!(
            update.release.release_url,
            "https://github.com/tools-rs/ramag/releases/tag/v1.1.0"
        );
        assert!(update.asset.is_none());
    }

    #[test]
    fn cached_update_notice_rejects_stale_or_noncanonical_versions() {
        let value = r#"{"current_version":"1.0.0","latest_version":"1.1.0"}"#;
        assert!(
            parse_cached_result("1.0.1", value)
                .expect("different app version should not be an error")
                .is_none()
        );

        let prerelease = r#"{"current_version":"1.0.0","latest_version":"1.1.0-beta.1"}"#;
        assert!(parse_cached_result("1.0.0", prerelease).is_err());
        assert!(parse_cached_result("1.0.0", &"x".repeat(257)).is_err());
    }
}
