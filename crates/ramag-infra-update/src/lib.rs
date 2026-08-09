#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! GitHub Release 更新检查、流式下载与 SHA-256 校验。

mod cache;
mod metadata;
mod platform;
mod runtime;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Response, StatusCode, Url, redirect};
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use ramag_domain::entities::{
    DownloadProgress, ReleaseAsset, ReleaseInfo, UpdateCancellation, UpdateProgressFn,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::UpdateDriver;

use crate::runtime::run_in_tokio;

const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/tools-rs/ramag/releases/download/";
const MAX_CHECKSUM_BYTES: usize = 64 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DOWNLOAD_DURATION: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct GitHubUpdateDriver {
    metadata_client: Client,
    download_client: Client,
    current_version: String,
}

impl GitHubUpdateDriver {
    pub fn new(current_version: &str) -> Result<Self> {
        Version::parse(current_version).map_err(|error| {
            DomainError::InvalidConfig(format!("当前应用版本无效 {current_version}：{error}"))
        })?;
        let user_agent = format!("Ramag/{current_version}");
        let metadata_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(trusted_redirect_policy())
            .user_agent(user_agent.clone())
            .build()
            .map_err(|error| DomainError::Other(format!("创建更新检查客户端失败：{error}")))?;
        let download_client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .redirect(trusted_redirect_policy())
            .user_agent(user_agent)
            .build()
            .map_err(|error| DomainError::Other(format!("创建更新下载客户端失败：{error}")))?;
        Ok(Self {
            metadata_client,
            download_client,
            current_version: current_version.to_string(),
        })
    }
}

#[async_trait]
impl UpdateDriver for GitHubUpdateDriver {
    async fn latest_stable_release(&self) -> Result<ReleaseInfo> {
        let client = self.metadata_client.clone();
        let current_version = self.current_version.clone();
        run_in_tokio(async move { metadata::fetch_latest_release(&client, &current_version).await })
            .await
    }

    async fn download_asset(
        &self,
        release: &ReleaseInfo,
        asset: &ReleaseAsset,
        progress: UpdateProgressFn,
        cancellation: UpdateCancellation,
    ) -> Result<PathBuf> {
        validate_release_and_asset(release, asset)?;
        let cache_dir = cache::update_cache_dir(&release.version)?;
        let client = self.download_client.clone();
        let release = release.clone();
        let asset = asset.clone();
        run_in_tokio(async move {
            download_verified_asset(&client, &release, &asset, cache_dir, progress, cancellation)
                .await
        })
        .await
    }

    fn reveal_download(&self, path: &Path) -> Result<()> {
        platform::reveal_download(path)
    }
}

fn trusted_redirect_policy() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("update request exceeded redirect limit");
        }
        if is_allowed_http_url(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("update request redirected to an untrusted host")
        }
    })
}

fn validate_release_and_asset(release: &ReleaseInfo, asset: &ReleaseAsset) -> Result<()> {
    let version = Version::parse(&release.version)
        .map_err(|error| DomainError::InvalidConfig(format!("更新版本无效：{error}")))?;
    let expected_tag = format!("v{version}");
    if release.tag_name != expected_tag {
        return Err(DomainError::InvalidConfig(
            "更新版本与 Release 标签不一致".into(),
        ));
    }
    if !is_safe_asset_name(&asset.name) || asset.size == 0 || asset.size > MAX_DOWNLOAD_BYTES {
        return Err(DomainError::InvalidConfig("更新资产元数据无效".into()));
    }
    validate_download_url(&asset.download_url, &release.tag_name, &asset.name)?;
    if let Some(hash) = &asset.sha256 {
        validate_sha256(hash)?;
    }
    Ok(())
}

async fn download_verified_asset(
    client: &Client,
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    cache_dir: PathBuf,
    progress: UpdateProgressFn,
    cancellation: UpdateCancellation,
) -> Result<PathBuf> {
    let expected_hash = match &asset.sha256 {
        Some(hash) => hash.clone(),
        None => fetch_checksum(client, &release.tag_name, &asset.name).await?,
    };
    prepare_cache_dir(&cache_dir).await?;
    let destination = cache_dir.join(&asset.name);
    let partial = cache_dir.join(format!("{}.part", asset.name));
    cache::reject_symlink(&destination).await?;
    cache::reject_symlink(&partial).await?;

    if path_exists(&destination).await? {
        if verify_file(&destination, asset.size, &expected_hash).await? {
            progress(DownloadProgress {
                downloaded: asset.size,
                total: asset.size,
            });
            return Ok(destination);
        }
        tokio::fs::remove_file(&destination)
            .await
            .map_err(|error| {
                DomainError::Storage(format!(
                    "清理无效更新缓存失败 {}：{error}",
                    destination.display()
                ))
            })?;
    }
    if path_exists(&partial).await? {
        tokio::fs::remove_file(&partial).await.map_err(|error| {
            DomainError::Storage(format!(
                "清理更新临时文件失败 {}：{error}",
                partial.display()
            ))
        })?;
    }

    let result = match tokio::time::timeout(
        MAX_DOWNLOAD_DURATION,
        download_to_partial(
            client,
            asset,
            &partial,
            &expected_hash,
            &progress,
            &cancellation,
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DomainError::ConnectionFailed(
            "安装包下载超过 30 分钟上限".into(),
        )),
    };
    if let Err(error) = result {
        if let Err(cleanup_error) = tokio::fs::remove_file(&partial).await
            && cleanup_error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(operation = "application_update_cleanup", error = %cleanup_error, path = %partial.display(), "cleanup partial update failed");
        }
        return Err(error);
    }
    tokio::fs::rename(&partial, &destination)
        .await
        .map_err(|error| {
            DomainError::Storage(format!(
                "提交已校验更新文件失败 {}：{error}",
                destination.display()
            ))
        })?;
    Ok(destination)
}

async fn prepare_cache_dir(cache_dir: &Path) -> Result<()> {
    let cache_parent = cache_dir
        .parent()
        .ok_or_else(|| DomainError::Storage("更新缓存目录缺少父目录".into()))?;
    cache::reject_symlink(cache_parent).await?;
    tokio::fs::create_dir_all(cache_dir)
        .await
        .map_err(|error| {
            DomainError::Storage(format!(
                "创建更新缓存目录失败 {}：{error}",
                cache_dir.display()
            ))
        })?;
    cache::reject_symlink(cache_parent).await?;
    cache::reject_symlink(cache_dir).await
}

async fn path_exists(path: &Path) -> Result<bool> {
    tokio::fs::try_exists(path).await.map_err(|error| {
        DomainError::Storage(format!("检查更新缓存路径失败 {}：{error}", path.display()))
    })
}

async fn download_to_partial(
    client: &Client,
    asset: &ReleaseAsset,
    partial: &Path,
    expected_hash: &str,
    progress: &UpdateProgressFn,
    cancellation: &UpdateCancellation,
) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(DomainError::Other("更新下载已取消".into()));
    }
    let mut response = tokio::time::timeout(
        DOWNLOAD_IDLE_TIMEOUT,
        client.get(&asset.download_url).send(),
    )
    .await
    .map_err(|_| DomainError::ConnectionFailed("等待安装包响应超过 30 秒".into()))?
    .map_err(|error| request_error("下载安装包", error))?;
    ensure_success(&response, "下载安装包")?;
    if !is_allowed_http_url(response.url()) {
        return Err(DomainError::ConnectionFailed(
            "安装包下载跳转到了不受信任的地址".into(),
        ));
    }
    if let Some(length) = response.content_length()
        && length != asset.size
    {
        return Err(DomainError::ConnectionFailed(format!(
            "安装包响应大小不匹配：期望 {} bytes，实际 {length} bytes",
            asset.size
        )));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)
        .await
        .map_err(|error| {
            DomainError::Storage(format!(
                "创建更新临时文件失败 {}：{error}",
                partial.display()
            ))
        })?;
    let mut downloaded = 0u64;
    let mut hasher = Sha256::new();
    progress(DownloadProgress {
        downloaded,
        total: asset.size,
    });
    while let Some(chunk) = tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, response.chunk())
        .await
        .map_err(|_| DomainError::ConnectionFailed("读取安装包超过 30 秒无数据".into()))?
        .map_err(|error| request_error("读取安装包", error))?
    {
        if cancellation.is_cancelled() {
            return Err(DomainError::Other("更新下载已取消".into()));
        }
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > asset.size || downloaded > MAX_DOWNLOAD_BYTES {
            return Err(DomainError::ConnectionFailed(
                "安装包实际大小超过发布元数据".into(),
            ));
        }
        file.write_all(&chunk).await.map_err(|error| {
            DomainError::Storage(format!(
                "写入更新临时文件失败 {}：{error}",
                partial.display()
            ))
        })?;
        hasher.update(&chunk);
        progress(DownloadProgress {
            downloaded,
            total: asset.size,
        });
    }
    if downloaded != asset.size {
        return Err(DomainError::ConnectionFailed(format!(
            "安装包下载不完整：期望 {} bytes，实际 {downloaded} bytes",
            asset.size
        )));
    }
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        return Err(DomainError::ConnectionFailed(format!(
            "安装包 SHA-256 校验失败：期望 {expected_hash}，实际 {actual_hash}"
        )));
    }
    file.sync_all().await.map_err(|error| {
        DomainError::Storage(format!(
            "同步更新临时文件失败 {}：{error}",
            partial.display()
        ))
    })?;
    Ok(())
}

async fn verify_file(path: &Path, expected_size: u64, expected_hash: &str) -> Result<bool> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        DomainError::Storage(format!("读取更新缓存信息失败 {}：{error}", path.display()))
    })?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Ok(false);
    }
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        DomainError::Storage(format!("读取更新缓存失败 {}：{error}", path.display()))
    })?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let count = file.read(&mut buffer).await.map_err(|error| {
            DomainError::Storage(format!("校验更新缓存失败 {}：{error}", path.display()))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()) == expected_hash)
}

async fn fetch_checksum(client: &Client, tag: &str, asset_name: &str) -> Result<String> {
    let url = format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/SHA256SUMS.txt");
    let response = tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| DomainError::ConnectionFailed("等待更新校验清单超过 30 秒".into()))?
        .map_err(|error| request_error("读取更新校验清单", error))?;
    let body = bounded_response_body(response, "读取更新校验清单", MAX_CHECKSUM_BYTES).await?;
    parse_checksum(&body, asset_name)
}

fn parse_checksum(body: &[u8], asset_name: &str) -> Result<String> {
    let text = std::str::from_utf8(body)
        .map_err(|error| DomainError::Other(format!("更新校验清单不是 UTF-8：{error}")))?;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else { continue };
        let Some(name) = fields.next() else { continue };
        if fields.next().is_some() {
            continue;
        }
        if name.trim_start_matches('*') == asset_name {
            validate_sha256(hash)?;
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(DomainError::NotFound(format!(
        "校验清单中缺少安装包：{asset_name}"
    )))
}

async fn bounded_response_body(
    mut response: Response,
    operation: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    ensure_success(&response, operation)?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(DomainError::ConnectionFailed(format!(
            "{operation}响应超过 {max_bytes} bytes 上限"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, response.chunk())
        .await
        .map_err(|_| DomainError::ConnectionFailed(format!("{operation}超过 30 秒无数据")))?
        .map_err(|error| request_error(operation, error))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(DomainError::ConnectionFailed(format!(
                "{operation}响应超过 {max_bytes} bytes 上限"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn ensure_success(response: &Response, operation: &str) -> Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let message = match response.status() {
        StatusCode::NOT_FOUND => format!("{operation}失败：没有已发布的稳定版本"),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            format!("{operation}失败：GitHub 请求频率受限，请稍后重试")
        }
        status => format!("{operation}失败：GitHub 返回 HTTP {status}"),
    };
    Err(DomainError::ConnectionFailed(message))
}

fn request_error(operation: &str, error: reqwest::Error) -> DomainError {
    if error.is_timeout() {
        DomainError::ConnectionFailed(format!("{operation}超时"))
    } else {
        DomainError::ConnectionFailed(format!("{operation}失败：{error}"))
    }
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(DomainError::Other("更新 SHA-256 格式无效".into()))
    }
}

fn validate_download_url(value: &str, tag: &str, asset_name: &str) -> Result<()> {
    let url = Url::parse(value)
        .map_err(|error| DomainError::Other(format!("更新下载地址无效：{error}")))?;
    let expected_path = format!("/tools-rs/ramag/releases/download/{tag}/{asset_name}");
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(DomainError::Other(format!(
            "更新下载地址不属于预期 Release：{value}"
        )));
    }
    Ok(())
}

fn is_allowed_http_url(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    url.host_str() == Some("github.com")
        || url
            .host_str()
            .is_some_and(|host| host.ends_with(".githubusercontent.com"))
}

fn is_safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
