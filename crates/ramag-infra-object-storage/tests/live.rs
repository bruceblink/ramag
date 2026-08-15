//! 真实云服务集成测试。仅在显式传入专用测试凭据并使用 `--ignored` 时运行。

use std::path::Path;
use std::sync::Arc;

use ramag_domain::entities::{
    CloudProvider, HttpsEndpoint, ObjectDownloadRequest, ObjectListQuery, ObjectStorageAccount,
    ObjectStorageMount, ObjectStorageMountId, ObjectUploadRequest, OverwritePolicy, SecretString,
    TransferCancellation, validate_root_prefix,
};
use ramag_domain::traits::ObjectStorageDriver;
use ramag_infra_object_storage::ObjectStorageInfra;

struct LiveConfig {
    provider: CloudProvider,
    access_key_id: String,
    access_key_secret: String,
    bucket: String,
    region: String,
    prefix: String,
}

impl LiveConfig {
    fn from_env(provider: CloudProvider) -> Option<Self> {
        let (id, secret, bucket, region, prefix) = match provider {
            CloudProvider::TencentCos => (
                "RAMAG_TEST_COS_SECRET_ID",
                "RAMAG_TEST_COS_SECRET_KEY",
                "RAMAG_TEST_COS_BUCKET",
                "RAMAG_TEST_COS_REGION",
                "RAMAG_TEST_COS_PREFIX",
            ),
            CloudProvider::AliyunOss => (
                "RAMAG_TEST_OSS_ACCESS_KEY_ID",
                "RAMAG_TEST_OSS_ACCESS_KEY_SECRET",
                "RAMAG_TEST_OSS_BUCKET",
                "RAMAG_TEST_OSS_REGION",
                "RAMAG_TEST_OSS_PREFIX",
            ),
        };
        Some(Self {
            provider,
            access_key_id: std::env::var(id).ok()?,
            access_key_secret: std::env::var(secret).ok()?,
            bucket: std::env::var(bucket).ok()?,
            region: std::env::var(region).ok()?,
            prefix: std::env::var(prefix).ok()?,
        })
    }
}

#[tokio::test]
#[ignore = "requires dedicated COS credentials and an explicit writable test prefix"]
async fn cos_live_round_trip() -> Result<(), String> {
    let Some(config) = LiveConfig::from_env(CloudProvider::TencentCos) else {
        return Ok(());
    };
    run_live(config).await
}

#[tokio::test]
#[ignore = "requires dedicated OSS credentials and an explicit writable test prefix"]
async fn oss_live_round_trip() -> Result<(), String> {
    let Some(config) = LiveConfig::from_env(CloudProvider::AliyunOss) else {
        return Ok(());
    };
    run_live(config).await
}

async fn run_live(config: LiveConfig) -> Result<(), String> {
    validate_root_prefix(&config.prefix)
        .map_err(|error| format!("test prefix must be a safe non-empty directory: {error}"))?;
    let mut account = ObjectStorageAccount::new("live-test", config.provider);
    account.read_only = false;
    account.access_key_id = SecretString::new(config.access_key_id);
    account.access_key_secret = SecretString::new(config.access_key_secret);
    let infra = ObjectStorageInfra::new().map_err(|error| error.to_string())?;
    let endpoint = match config.provider {
        CloudProvider::TencentCos => format!("https://cos.{}.myqcloud.com", config.region),
        CloudProvider::AliyunOss => format!("https://oss-{}.aliyuncs.com", config.region),
    };
    let mount = ObjectStorageMount {
        id: ObjectStorageMountId::new(),
        account_id: account.id.clone(),
        bucket: config.bucket.clone(),
        region: config.region.clone(),
        endpoint: HttpsEndpoint::parse_official(config.provider, &endpoint)
            .map_err(|error| error.to_string())?,
        root_prefix: None,
        created_at: None,
        storage_class: None,
    };
    let query = ObjectListQuery::new(&config.prefix, "").map_err(|error| error.to_string())?;
    ObjectStorageDriver::list_page(&infra, &account.snapshot(), &mount, &query, None, 1)
        .await
        .map_err(|error| error.to_string())?;

    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let key = format!(
        "{}ramag-live-{}.txt",
        config.prefix,
        uuid::Uuid::new_v4().simple()
    );
    let source = directory.path().join("source.txt");
    std::fs::write(&source, b"ramag object storage live test\n")
        .map_err(|error| error.to_string())?;
    let operation =
        exercise_object(&infra, &account, &mount, &key, &source, directory.path()).await;
    let cleanup = ObjectStorageDriver::delete(&infra, &account.snapshot(), &mount, &key).await;
    let shutdown = ObjectStorageDriver::shutdown(&infra).await;
    match (operation, cleanup, shutdown) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (operation, cleanup, shutdown) => Err(format!(
            "live operation failed: operation={operation:?}, cleanup={cleanup:?}, shutdown={shutdown:?}; residual key may exist: {key}"
        )),
    }
}

async fn exercise_object(
    infra: &ObjectStorageInfra,
    account: &ObjectStorageAccount,
    mount: &ObjectStorageMount,
    key: &str,
    source: &Path,
    directory: &Path,
) -> Result<(), String> {
    let upload = |account: &ObjectStorageAccount, overwrite| ObjectUploadRequest {
        account: account.snapshot(),
        mount: mount.clone(),
        key: key.to_string(),
        local_path: source.to_path_buf(),
        overwrite,
        cancellation: TransferCancellation::default(),
        progress: Arc::new(|_| {}),
    };
    ObjectStorageDriver::upload(infra, upload(account, OverwritePolicy::Refuse))
        .await
        .map_err(|error| error.to_string())?;
    if ObjectStorageDriver::upload(infra, upload(account, OverwritePolicy::Refuse))
        .await
        .is_ok()
    {
        return Err("second no-overwrite upload unexpectedly succeeded".into());
    }
    let metadata = ObjectStorageDriver::stat(infra, &account.snapshot(), mount, key)
        .await
        .map_err(|error| error.to_string())?;
    if metadata.size == 0 {
        return Err("uploaded object unexpectedly has zero size".into());
    }
    let target = directory.join("download.txt");
    ObjectStorageDriver::download(
        infra,
        ObjectDownloadRequest {
            account: account.snapshot(),
            mount: mount.clone(),
            key: key.to_string(),
            local_path: target.clone(),
            overwrite: OverwritePolicy::Refuse,
            cancellation: TransferCancellation::default(),
            progress: Arc::new(|_| {}),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    if std::fs::read(&target).map_err(|error| error.to_string())?
        != std::fs::read(source).map_err(|error| error.to_string())?
    {
        return Err("downloaded content does not match upload source".into());
    }
    let mut read_only = account.clone();
    read_only.read_only = true;
    if ObjectStorageDriver::upload(infra, upload(&read_only, OverwritePolicy::Overwrite))
        .await
        .is_ok()
    {
        return Err("read-only upload unexpectedly reached the provider".into());
    }
    Ok(())
}
