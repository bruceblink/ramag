//! 对象列表、元数据与传输操作。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::future::{Either, select};
use ramag_domain::entities::{
    MAX_OBJECT_STORAGE_QUEUED_TRANSFERS, ObjectCapabilities, ObjectDownloadRequest,
    ObjectListCursor, ObjectListQuery, ObjectMetadata, ObjectProgressFn, ObjectStorageAccountId,
    ObjectStorageMount, ObjectTextPreview, ObjectUploadRequest, OverwritePolicy,
    TransferCancellation,
};
use ramag_domain::error::{DomainError, READ_ONLY_MESSAGE, Result};

use super::{ObjectListingPage, ObjectStorageService, log_object_storage_error};

impl ObjectStorageService {
    pub async fn capabilities(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
    ) -> Result<ObjectCapabilities> {
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            self.driver
                .capabilities(&account.snapshot(), mount)
                .await
                .map_err(DomainError::from)
        }
        .await;
        log_object_storage_error("object_storage_capabilities", Some(account_id), &result);
        result
    }

    pub async fn start_listing(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        prefix: &str,
        name_prefix: &str,
    ) -> Result<ObjectListingPage> {
        let result = async {
            let query =
                ObjectListQuery::new(prefix, name_prefix).map_err(DomainError::InvalidConfig)?;
            let generation = self.advance_listing_generation(account_id, mount);
            self.list_page(account_id, mount, &query, None, generation)
                .await
        }
        .await;
        log_object_storage_error("object_storage_list_start", Some(account_id), &result);
        result
    }

    pub async fn continue_listing(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        prefix: &str,
        name_prefix: &str,
        cursor: &ObjectListCursor,
        generation: u64,
    ) -> Result<ObjectListingPage> {
        let result = async {
            let current = self.current_listing_generation(account_id, mount);
            if current != Some(generation) {
                return Err(DomainError::InvalidConfig(
                    "对象列表上下文已变化，请重新加载".into(),
                ));
            }
            let query =
                ObjectListQuery::new(prefix, name_prefix).map_err(DomainError::InvalidConfig)?;
            self.list_page(account_id, mount, &query, Some(cursor), generation)
                .await
        }
        .await;
        log_object_storage_error("object_storage_list_continue", Some(account_id), &result);
        result
    }

    pub async fn stat_object(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> Result<ObjectMetadata> {
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            self.driver
                .stat(&account.snapshot(), mount, key)
                .await
                .map_err(DomainError::from)
        }
        .await;
        log_object_storage_error("object_storage_object_stat", Some(account_id), &result);
        result
    }

    pub async fn preview_text_object(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> Result<ObjectTextPreview> {
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            self.driver
                .read_text_preview(&account.snapshot(), mount, key)
                .await
                .map_err(DomainError::from)
        }
        .await;
        log_object_storage_error("object_storage_object_preview", Some(account_id), &result);
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upload_object(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        key: String,
        local_path: PathBuf,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: ObjectProgressFn,
    ) -> Result<()> {
        let transfer_id = self.register_transfer(account_id, cancellation.clone());
        let _permit = match self.acquire_transfer_slot(&cancellation).await {
            Ok(permit) => permit,
            Err(error) => {
                self.unregister_transfer(account_id, transfer_id);
                let result: Result<()> = Err(error);
                log_object_storage_error("object_storage_upload", Some(account_id), &result);
                return result;
            }
        };
        if cancellation.is_cancelled() {
            self.unregister_transfer(account_id, transfer_id);
            let result = Err(transfer_cancelled());
            log_object_storage_error("object_storage_upload", Some(account_id), &result);
            return result;
        }
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            if cancellation.is_cancelled() {
                return Err(transfer_cancelled());
            }
            if account.read_only {
                return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
            }
            self.driver
                .upload(ObjectUploadRequest {
                    account: account.snapshot(),
                    mount: mount.clone(),
                    key,
                    local_path,
                    overwrite,
                    cancellation,
                    progress,
                })
                .await
                .map_err(DomainError::from)
        }
        .await;
        self.unregister_transfer(account_id, transfer_id);
        log_object_storage_error("object_storage_upload", Some(account_id), &result);
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn download_object(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        key: String,
        local_path: PathBuf,
        overwrite: OverwritePolicy,
        cancellation: TransferCancellation,
        progress: ObjectProgressFn,
    ) -> Result<()> {
        let transfer_id = self.register_transfer(account_id, cancellation.clone());
        let _permit = match self.acquire_transfer_slot(&cancellation).await {
            Ok(permit) => permit,
            Err(error) => {
                self.unregister_transfer(account_id, transfer_id);
                let result: Result<()> = Err(error);
                log_object_storage_error("object_storage_download", Some(account_id), &result);
                return result;
            }
        };
        if cancellation.is_cancelled() {
            self.unregister_transfer(account_id, transfer_id);
            let result = Err(transfer_cancelled());
            log_object_storage_error("object_storage_download", Some(account_id), &result);
            return result;
        }
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            if cancellation.is_cancelled() {
                return Err(transfer_cancelled());
            }
            self.driver
                .download(ObjectDownloadRequest {
                    account: account.snapshot(),
                    mount: mount.clone(),
                    key,
                    local_path,
                    overwrite,
                    cancellation,
                    progress,
                })
                .await
                .map_err(DomainError::from)
        }
        .await;
        self.unregister_transfer(account_id, transfer_id);
        log_object_storage_error("object_storage_download", Some(account_id), &result);
        result
    }

    pub async fn delete_object(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> Result<()> {
        let result = async {
            let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
            if account.read_only {
                return Err(DomainError::Forbidden(READ_ONLY_MESSAGE.into()));
            }
            self.driver
                .delete(&account.snapshot(), mount, key)
                .await
                .map_err(DomainError::from)
        }
        .await;
        log_object_storage_error("object_storage_object_delete", Some(account_id), &result);
        result
    }

    async fn list_page(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
        query: &ObjectListQuery,
        cursor: Option<&ObjectListCursor>,
        generation: u64,
    ) -> Result<ObjectListingPage> {
        let (account, _guard) = self.lock_account_for_mount(account_id, mount).await?;
        let page = self
            .driver
            .list_page(&account.snapshot(), mount, query, cursor, generation)
            .await?;
        Ok(ObjectListingPage { generation, page })
    }

    async fn lock_account_for_mount(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
    ) -> Result<(
        ramag_domain::entities::ObjectStorageAccount,
        tokio::sync::OwnedRwLockReadGuard<()>,
    )> {
        if &mount.account_id != account_id {
            return Err(DomainError::InvalidConfig(
                "挂载点不属于当前对象存储账号".into(),
            ));
        }
        let guard = self.account_gate(account_id).read_owned().await;
        let account = self.load_account(account_id).await?;
        Ok((account, guard))
    }

    fn advance_listing_generation(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
    ) -> u64 {
        let key = listing_key(account_id, mount);
        let mut generations = self.listing_generations.lock();
        let generation = generations.entry(key).or_default();
        *generation = generation.wrapping_add(1).max(1);
        *generation
    }

    fn current_listing_generation(
        &self,
        account_id: &ObjectStorageAccountId,
        mount: &ObjectStorageMount,
    ) -> Option<u64> {
        self.listing_generations
            .lock()
            .get(&listing_key(account_id, mount))
            .copied()
    }

    async fn acquire_transfer_slot(
        &self,
        cancellation: &TransferCancellation,
    ) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.queued_transfers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                (queued < MAX_OBJECT_STORAGE_QUEUED_TRANSFERS).then_some(queued + 1)
            })
            .map_err(|_| {
                DomainError::Other(format!(
                    "等待传输已达 {MAX_OBJECT_STORAGE_QUEUED_TRANSFERS} 个上限"
                ))
            })?;
        let acquire = Box::pin(self.transfer_slots.clone().acquire_owned());
        let cancellation = cancellation.clone();
        let cancelled = Box::pin(async move {
            while !cancellation.is_cancelled() {
                smol::Timer::after(Duration::from_millis(50)).await;
            }
        });
        let permit = match select(acquire, cancelled).await {
            Either::Left((permit, _)) => permit,
            Either::Right(_) => {
                self.queued_transfers.fetch_sub(1, Ordering::AcqRel);
                return Err(transfer_cancelled());
            }
        };
        self.queued_transfers.fetch_sub(1, Ordering::AcqRel);
        permit.map_err(|_| DomainError::Other("对象存储传输队列已停止".into()))
    }
}

fn transfer_cancelled() -> DomainError {
    DomainError::ObjectStorage(ramag_domain::error::ObjectStorageError::new(
        ramag_domain::error::ObjectStorageErrorCategory::Cancelled,
        "transfer_queue",
        "操作已取消",
    ))
}

fn listing_key(account_id: &ObjectStorageAccountId, mount: &ObjectStorageMount) -> String {
    format!("{account_id}:{}", mount.id)
}
