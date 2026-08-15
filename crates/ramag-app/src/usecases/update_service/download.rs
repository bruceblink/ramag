use super::*;

impl UpdateService {
    pub async fn download(
        &self,
        update: &AvailableUpdate,
        progress: UpdateProgressFn,
        cancellation: UpdateCancellation,
    ) -> Result<std::path::PathBuf> {
        let Some(asset) = update.asset.as_ref() else {
            warn!(
                operation = "update_download_validation",
                version = %update.release.version,
                reason = "asset_missing",
                "update asset is unavailable for the current platform"
            );
            return Err(DomainError::NotImplemented(
                "当前平台没有可下载的安装包".into(),
            ));
        };
        let Some(platform) = current_platform() else {
            warn!(
                operation = "update_download_validation",
                version = %update.release.version,
                asset = %asset.name,
                reason = "platform_unknown",
                "current platform is not supported for update download"
            );
            return Err(DomainError::NotImplemented(
                "当前平台没有可下载的安装包".into(),
            ));
        };
        let expected_name = asset_name_for(platform, &update.release.version);
        if asset.name != expected_name {
            warn!(
                operation = "update_download_validation",
                version = %update.release.version,
                asset = %asset.name,
                expected_asset = %expected_name,
                reason = "asset_mismatch",
                "update asset does not match the current platform"
            );
            return Err(DomainError::InvalidConfig(format!(
                "更新资产与当前平台不匹配：{}",
                asset.name
            )));
        }
        let current = match Version::parse(&self.current_version) {
            Ok(version) => version,
            Err(error) => {
                warn!(
                    operation = "update_download_validation",
                    current_version = %self.current_version,
                    error = %error,
                    reason = "current_version_invalid",
                    "current application version is invalid"
                );
                return Err(DomainError::InvalidConfig(format!(
                    "当前应用版本无效：{error}"
                )));
            }
        };
        let latest = match Version::parse(&update.release.version) {
            Ok(version) => version,
            Err(error) => {
                warn!(
                    operation = "update_download_validation",
                    version = %update.release.version,
                    error = %error,
                    reason = "release_version_invalid",
                    "update release version is invalid"
                );
                return Err(DomainError::InvalidConfig(format!("更新版本无效：{error}")));
            }
        };
        if latest <= current {
            warn!(
                operation = "update_download_validation",
                current_version = %current,
                version = %latest,
                reason = "not_newer",
                "update release is not newer than the current version"
            );
            return Err(DomainError::Other("更新版本不高于当前版本".into()));
        }
        info!(
            operation = "update_download",
            version = %update.release.version,
            asset = %asset.name,
            expected_bytes = asset.size,
            "update download started"
        );
        let result = self
            .driver
            .download_asset(&update.release, asset, progress, cancellation)
            .await;
        match &result {
            Ok(path) => {
                info!(
                    operation = "update_download",
                    version = %update.release.version,
                    asset = %asset.name,
                    path = %path.display(),
                    "update download completed"
                )
            }
            Err(download_error) => {
                error!(
                    operation = "update_download",
                    error = %download_error,
                    version = %update.release.version,
                    asset = %asset.name,
                    expected_bytes = asset.size,
                    "update download failed"
                )
            }
        }
        result
    }
}
