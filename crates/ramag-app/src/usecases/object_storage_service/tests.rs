use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use ramag_domain::entities::{
    CloudProvider, ManualBucket, ObjectCapabilities, ObjectDownloadRequest, ObjectListCursor,
    ObjectListQuery, ObjectMetadata, ObjectPage, ObjectStorageAccount, ObjectStorageAccountId,
    ObjectStorageAccountSnapshot, ObjectStorageFavorite, ObjectStorageMount, ObjectStorageMountId,
    ObjectStorageSessionPreference, ObjectStorageWorkspacePreference, ObjectStorageWorkspaceState,
    ObjectTextPreview, ObjectUploadRequest, OverwritePolicy, SecretString, TransferCancellation,
};
use ramag_domain::error::{ObjectStorageError, ObjectStorageErrorCategory, ObjectStorageResult};
use ramag_domain::traits::{ObjectStorageDriver, Storage};
use ramag_infra_storage::RedbStorage;

use super::{AccountVerification, ObjectStorageService};

#[derive(Default)]
struct FakeDriver {
    lists: AtomicUsize,
    list_error: Option<ObjectStorageError>,
    uploads: AtomicUsize,
    deletes: AtomicUsize,
    invalidations: AtomicUsize,
    block_uploads_until_cancelled: bool,
}

#[async_trait]
impl ObjectStorageDriver for FakeDriver {
    async fn capabilities(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities> {
        Ok(ObjectCapabilities {
            stat: true,
            read: true,
            write: true,
            delete: true,
            list: true,
            atomic_create: true,
        })
    }

    async fn list_page(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _query: &ObjectListQuery,
        _cursor: Option<&ObjectListCursor>,
        _request_generation: u64,
    ) -> ObjectStorageResult<ObjectPage> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = &self.list_error {
            return Err(error.clone());
        }
        Ok(ObjectPage {
            entries: Vec::new(),
            next_cursor: None,
            capped: false,
        })
    }

    async fn stat(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectMetadata> {
        Ok(ObjectMetadata {
            key: key.into(),
            size: 0,
            last_modified: None,
            etag: None,
            version: None,
            content_type: None,
            user_metadata: Vec::new(),
            storage_class: None,
        })
    }

    async fn read_text_preview(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _key: &str,
    ) -> ObjectStorageResult<ObjectTextPreview> {
        Ok(ObjectTextPreview {
            content: "preview".into(),
            total_bytes: 7,
            truncated: false,
        })
    }

    async fn upload(&self, request: ObjectUploadRequest) -> ObjectStorageResult<()> {
        self.uploads.fetch_add(1, Ordering::Relaxed);
        if self.block_uploads_until_cancelled {
            while !request.cancellation.is_cancelled() {
                smol::Timer::after(Duration::from_millis(5)).await;
            }
            return Err(ObjectStorageError::new(
                ObjectStorageErrorCategory::Cancelled,
                "upload",
                "cancelled",
            ));
        }
        Ok(())
    }

    async fn download(&self, _request: ObjectDownloadRequest) -> ObjectStorageResult<()> {
        Ok(())
    }

    async fn delete(
        &self,
        _account: &ObjectStorageAccountSnapshot,
        _mount: &ObjectStorageMount,
        _key: &str,
    ) -> ObjectStorageResult<()> {
        self.deletes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn invalidate_account(
        &self,
        _account_id: &ObjectStorageAccountId,
        _minimum_revision: u64,
    ) -> ObjectStorageResult<()> {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn shutdown(&self) -> ObjectStorageResult<()> {
        Ok(())
    }
}

fn account() -> ObjectStorageAccount {
    let mut account = ObjectStorageAccount::new("test-account", CloudProvider::AliyunOss);
    account.access_key_id = SecretString::new("test-access-key");
    account.access_key_secret = SecretString::new("test-secret-key");
    account.manual_buckets = vec![ManualBucket::new("valid-bucket", "cn-hangzhou")];
    account
}

fn storage() -> (tempfile::TempDir, Arc<RedbStorage>) {
    let directory = tempfile::tempdir().expect("create temp directory");
    let storage = RedbStorage::open_with_key(&directory.path().join("ramag.redb"), &[7; 32])
        .expect("open test storage");
    (directory, Arc::new(storage))
}

#[tokio::test]
async fn configured_bucket_is_verified_and_saved() {
    let (_directory, storage) = storage();
    let driver = Arc::new(FakeDriver::default());
    let service = ObjectStorageService::new(driver.clone(), storage.clone());

    let saved = service.save_account(account()).await.expect("save account");

    assert_eq!(saved.verification, AccountVerification::Verified);
    assert!(
        storage
            .get_object_storage_account(&saved.account.id)
            .await
            .expect("load")
            .is_some()
    );
    assert_eq!(driver.lists.load(Ordering::Relaxed), 1);
    assert_eq!(driver.invalidations.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn missing_bucket_is_rejected_before_save() {
    let (_directory, storage) = storage();
    let driver = Arc::new(FakeDriver::default());
    let service = ObjectStorageService::new(driver.clone(), storage.clone());
    let mut rejected = account();
    rejected.manual_buckets.clear();
    let rejected_id = rejected.id.clone();

    assert!(service.save_account(rejected).await.is_err());
    assert_eq!(driver.lists.load(Ordering::Relaxed), 0);
    assert!(
        storage
            .get_object_storage_account(&rejected_id)
            .await
            .expect("load")
            .is_none()
    );
}

#[tokio::test]
async fn invalid_credentials_from_configured_bucket_reject_save() {
    let (_directory, storage) = storage();
    let invalid_error = ObjectStorageError::new(
        ObjectStorageErrorCategory::InvalidCredentials,
        "list",
        "invalid credentials",
    );
    let rejected = account();
    let rejected_id = rejected.id.clone();
    let service = ObjectStorageService::new(
        Arc::new(FakeDriver {
            list_error: Some(invalid_error),
            ..FakeDriver::default()
        }),
        storage.clone(),
    );
    assert!(service.save_account(rejected).await.is_err());
    assert!(
        storage
            .get_object_storage_account(&rejected_id)
            .await
            .expect("load")
            .is_none()
    );
}

#[tokio::test]
async fn configured_mounts_are_loaded_without_remote_bucket_discovery() {
    let (_directory, storage) = storage();
    let account = account();
    storage
        .save_object_storage_account(&account)
        .await
        .expect("seed account");
    let driver = Arc::new(FakeDriver::default());
    let service = ObjectStorageService::new(driver.clone(), storage);

    let result = service.list_mounts(&account.id).await.expect("list mounts");

    assert_eq!(result.mounts.len(), 1);
    assert_eq!(result.mounts[0].bucket, "valid-bucket");
    assert_eq!(driver.lists.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn production_mode_rejects_write_before_driver_call() {
    let (_directory, storage) = storage();
    let mut account = account();
    account.read_only = true;
    storage
        .save_object_storage_account(&account)
        .await
        .expect("seed account");
    let driver = Arc::new(FakeDriver::default());
    let service = ObjectStorageService::new(driver.clone(), storage);
    let mounts = super::configured_mounts(&account).expect("configured mounts");
    assert_eq!(mounts.len(), 1);

    let upload = service
        .upload_object(
            &account.id,
            &ObjectStorageMount {
                id: ramag_domain::entities::ObjectStorageMountId::new(),
                account_id: account.id.clone(),
                bucket: "valid-bucket".into(),
                region: "cn-hangzhou".into(),
                endpoint: ramag_domain::entities::HttpsEndpoint::parse_official(
                    CloudProvider::AliyunOss,
                    "https://oss-cn-hangzhou.aliyuncs.com",
                )
                .expect("official endpoint"),
                root_prefix: None,
                created_at: None,
                storage_class: None,
            },
            "object.txt".into(),
            PathBuf::from("/tmp/not-read.txt"),
            OverwritePolicy::Refuse,
            TransferCancellation::default(),
            Arc::new(|_| {}),
        )
        .await;
    assert!(upload.is_err());
    assert_eq!(driver.uploads.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn workspace_round_trip_is_encrypted_and_restores_navigation_state() {
    let (_directory, storage) = storage();
    let account = account();
    storage
        .save_object_storage_account(&account)
        .await
        .expect("seed account");
    let service = ObjectStorageService::new(Arc::new(FakeDriver::default()), storage.clone());
    let mount_id = ObjectStorageMountId::new();
    let preference = ObjectStorageWorkspacePreference {
        active_account_id: Some(account.id.clone()),
        workspaces: vec![ObjectStorageWorkspaceState {
            account_id: account.id.clone(),
            mount_id: Some(mount_id.clone()),
            prefix: "team/reports/".into(),
        }],
        favorites: vec![ObjectStorageFavorite {
            account_id: account.id.clone(),
            mount_id,
            prefix: "team/reports/".into(),
        }],
        show_mounts: true,
        show_detail: true,
    };

    service
        .save_workspace(&account.id, &preference)
        .await
        .expect("save workspace");

    let raw = storage
        .get_preference(&format!("object_storage.workspace.{}", account.id))
        .await
        .expect("load raw preference")
        .expect("preference exists");
    assert!(raw.starts_with("enc-v1:"));
    assert!(!raw.contains("team/reports"));
    assert_eq!(
        service
            .load_workspace(&account.id)
            .await
            .expect("load workspace"),
        preference
    );
}

#[tokio::test]
async fn session_round_trip_is_encrypted_and_rejects_duplicate_accounts() {
    let (_directory, storage) = storage();
    let service = ObjectStorageService::new(Arc::new(FakeDriver::default()), storage.clone());
    let account_id = ObjectStorageAccountId::new();
    let preference = ObjectStorageSessionPreference {
        open_account_ids: vec![account_id.clone()],
        active_account_id: Some(account_id.clone()),
    };

    service
        .save_session_preference(&preference)
        .await
        .expect("save sessions");

    let raw = storage
        .get_preference("object_storage.sessions")
        .await
        .expect("load raw preference")
        .expect("preference exists");
    assert!(raw.starts_with("enc-v1:"));
    assert!(!raw.contains(&account_id.to_string()));
    assert_eq!(
        service
            .load_session_preference()
            .await
            .expect("load sessions"),
        preference
    );

    let duplicate = ObjectStorageSessionPreference {
        open_account_ids: vec![account_id.clone(), account_id],
        active_account_id: None,
    };
    assert!(service.save_session_preference(&duplicate).await.is_err());
}

#[tokio::test]
async fn stale_account_revision_cannot_overwrite_newer_configuration() {
    let (_directory, storage) = storage();
    let service = ObjectStorageService::new(Arc::new(FakeDriver::default()), storage.clone());
    let original = service
        .save_account(account())
        .await
        .expect("save account")
        .account;
    let stale = original.clone();
    let mut updated = original;
    updated.name = "updated".into();
    let updated = service
        .save_account(updated)
        .await
        .expect("update account")
        .account;

    assert!(service.save_account(stale).await.is_err());
    assert_eq!(
        storage
            .get_object_storage_account(&updated.id)
            .await
            .expect("load account"),
        Some(updated)
    );
}

#[tokio::test]
async fn closing_session_cancels_active_and_queued_transfers_before_invalidation() {
    let (_directory, storage) = storage();
    let mut account = account();
    account.read_only = false;
    storage
        .save_object_storage_account(&account)
        .await
        .expect("seed account");
    let driver = Arc::new(FakeDriver {
        block_uploads_until_cancelled: true,
        ..FakeDriver::default()
    });
    let service = ObjectStorageService::new(driver.clone(), storage);
    let mount = ObjectStorageMount {
        id: ObjectStorageMountId::new(),
        account_id: account.id.clone(),
        bucket: "valid-bucket".into(),
        region: "cn-hangzhou".into(),
        endpoint: ramag_domain::entities::HttpsEndpoint::parse_official(
            CloudProvider::AliyunOss,
            "https://oss-cn-hangzhou.aliyuncs.com",
        )
        .expect("official endpoint"),
        root_prefix: None,
        created_at: None,
        storage_class: None,
    };
    let upload = |index| {
        service.upload_object(
            &account.id,
            &mount,
            format!("object-{index}.txt"),
            PathBuf::from(format!("/tmp/object-{index}.txt")),
            OverwritePolicy::Refuse,
            TransferCancellation::default(),
            Arc::new(|_| {}),
        )
    };
    let close = async {
        while driver.uploads.load(Ordering::Relaxed) < 3 {
            smol::Timer::after(Duration::from_millis(5)).await;
        }
        service.close_account_session(&account.id).await
    };

    let (first, second, third, queued, close_result) =
        futures::join!(upload(1), upload(2), upload(3), upload(4), close);

    for result in [first, second, third, queued] {
        assert!(matches!(
            result,
            Err(ramag_domain::error::DomainError::ObjectStorage(error))
                if error.category == ObjectStorageErrorCategory::Cancelled
        ));
    }
    close_result.expect("close session");
    assert_eq!(driver.uploads.load(Ordering::Relaxed), 3);
    assert_eq!(driver.invalidations.load(Ordering::Relaxed), 1);
}
