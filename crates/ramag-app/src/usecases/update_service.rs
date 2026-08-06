//! 应用更新用例：版本比较、检查节流与平台安装包选择。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::lock::Mutex;
use parking_lot::RwLock;
use ramag_domain::entities::{ReleaseAsset, ReleaseInfo, UpdateCancellation, UpdateProgressFn};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{Storage, UpdateDriver};
use semver::Version;
use tracing::warn;

pub const UPDATE_CHECK_PREF_KEY: &str = "update_last_check_at_v1";
pub const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// 自动检查因本地节流策略被跳过；手动检查不会返回此状态。
    Skipped,
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
        if !force && !self.should_check().await {
            return Ok(UpdateCheckResult::Skipped);
        }
        self.mark_check_attempt().await;

        let release = self.driver.latest_stable_release().await?;
        let result = evaluate_release(&self.current_version, release, current_platform())?;
        *self.last_result.write() = Some(result.clone());
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
        self.driver
            .download_asset(&update.release, asset, progress, cancellation)
            .await
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

    use super::{UpdateCheckResult, UpdatePlatform, asset_name_for, evaluate_release};

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
}
