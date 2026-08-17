//! 对象存储账号、对象与传输服务。

mod mounts;
mod operations;
#[cfg(test)]
mod tests;
mod workspace;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::{Either, select};
use parking_lot::Mutex;
use ramag_domain::entities::{
    ObjectListQuery, ObjectPage, ObjectStorageAccount, ObjectStorageAccountId, ObjectStorageMount,
};
use ramag_domain::error::{DomainError, ObjectStorageError, ObjectStorageErrorCategory, Result};
use ramag_domain::traits::{ObjectStorageDriver, Storage};

pub use mounts::configured_mounts;

const WORKSPACE_PREFERENCE_PREFIX: &str = "object_storage.workspace.";
const SESSION_PREFERENCE_KEY: &str = "object_storage.sessions";
const ENCRYPTED_WORKSPACE_PREFIX: &str = "enc-v1:";
const MAX_ENCRYPTED_WORKSPACE_BYTES: usize = 2 * 64 * 1024 * 1024 + 1024;
const MAX_ENCRYPTED_SESSION_BYTES: usize = 2 * 16 * 1024 + 1024;
const ACCOUNT_WRITE_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountVerification {
    Verified,
    Unverified { reason: String },
}

#[derive(Debug, Clone)]
pub struct SavedObjectStorageAccount {
    pub account: ObjectStorageAccount,
    pub verification: AccountVerification,
}

#[derive(Debug, Clone)]
pub struct ObjectStorageMountResult {
    pub mounts: Vec<ObjectStorageMount>,
}

#[derive(Debug, Clone)]
pub struct ObjectListingPage {
    pub generation: u64,
    pub page: ObjectPage,
}

pub struct ObjectStorageService {
    pub(super) driver: Arc<dyn ObjectStorageDriver>,
    pub(super) storage: Arc<dyn Storage>,
    account_gates: Mutex<HashMap<ObjectStorageAccountId, Arc<tokio::sync::RwLock<()>>>>,
    listing_generations: Mutex<HashMap<String, u64>>,
    transfer_slots: Arc<tokio::sync::Semaphore>,
    queued_transfers: AtomicUsize,
    active_transfers: Mutex<
        HashMap<ObjectStorageAccountId, HashMap<u64, ramag_domain::entities::TransferCancellation>>,
    >,
    next_transfer_id: AtomicU64,
}

impl ObjectStorageService {
    pub fn new(driver: Arc<dyn ObjectStorageDriver>, storage: Arc<dyn Storage>) -> Self {
        Self {
            driver,
            storage,
            account_gates: Mutex::new(HashMap::new()),
            listing_generations: Mutex::new(HashMap::new()),
            transfer_slots: Arc::new(tokio::sync::Semaphore::new(
                ramag_domain::entities::MAX_OBJECT_STORAGE_CONCURRENT_TRANSFERS,
            )),
            queued_transfers: AtomicUsize::new(0),
            active_transfers: Mutex::new(HashMap::new()),
            next_transfer_id: AtomicU64::new(1),
        }
    }

    pub async fn list_accounts(&self) -> Result<Vec<ObjectStorageAccount>> {
        let result = self.storage.list_object_storage_accounts().await;
        log_object_storage_error("object_storage_account_list", None, &result);
        result
    }

    pub async fn get_account(&self, id: &ObjectStorageAccountId) -> Result<ObjectStorageAccount> {
        let result = self.load_account(id).await;
        log_object_storage_error("object_storage_account_get", Some(id), &result);
        result
    }

    pub(super) async fn load_account(
        &self,
        id: &ObjectStorageAccountId,
    ) -> Result<ObjectStorageAccount> {
        match self.storage.get_object_storage_account(id).await {
            Ok(Some(account)) => Ok(account),
            Ok(None) => Err(DomainError::NotFound(format!("对象存储账号 {id}"))),
            Err(error) => Err(error),
        }
    }

    pub async fn verify_account(
        &self,
        account: &ObjectStorageAccount,
    ) -> Result<AccountVerification> {
        let result = async {
            account.validate().map_err(DomainError::InvalidConfig)?;
            self.verify_configured_mounts(account).await
        }
        .await;
        log_object_storage_error("object_storage_account_verify", Some(&account.id), &result);
        result
    }

    pub async fn save_account(
        &self,
        mut account: ObjectStorageAccount,
    ) -> Result<SavedObjectStorageAccount> {
        let account_id = account.id.clone();
        let result = async {
            normalize_manual_mounts(&mut account);
            account.validate().map_err(DomainError::InvalidConfig)?;
            self.cancel_account_transfers(&account.id);
            let _guard = self.acquire_account_write_guard(&account.id).await?;
            let existing = self.storage.get_object_storage_account(&account.id).await?;
            match existing {
                Some(existing) if existing.revision != account.revision => {
                    return Err(DomainError::InvalidConfig(
                        "账号已被其他操作更新，请刷新后重试".into(),
                    ));
                }
                Some(existing) if existing.revision == u64::MAX => {
                    return Err(DomainError::InvalidConfig(
                        "账号 revision 已耗尽，请新建账号后迁移配置".into(),
                    ));
                }
                Some(existing) => account.revision = existing.next_revision(),
                None if account.revision != 1 => {
                    return Err(DomainError::InvalidConfig(
                        "新账号 revision 必须为 1".into(),
                    ));
                }
                None => {}
            }
            let verification = self.verify_configured_mounts(&account).await?;
            self.storage.save_object_storage_account(&account).await?;
            self.driver
                .invalidate_account(&account.id, account.revision)
                .await?;
            self.clear_account_state(&account.id);
            Ok(SavedObjectStorageAccount {
                account,
                verification,
            })
        }
        .await;
        log_object_storage_error("object_storage_account_save", Some(&account_id), &result);
        result
    }

    pub async fn delete_account(&self, id: &ObjectStorageAccountId) -> Result<()> {
        let result = async {
            self.cancel_account_transfers(id);
            let _guard = self.acquire_account_write_guard(id).await?;
            self.storage
                .delete_object_storage_account(id, &workspace_preference_key(id))
                .await?;
            self.driver.invalidate_account(id, u64::MAX).await?;
            self.clear_account_state(id);
            Ok(())
        }
        .await;
        log_object_storage_error("object_storage_account_delete", Some(id), &result);
        result
    }

    pub async fn close_account_session(&self, id: &ObjectStorageAccountId) -> Result<()> {
        let result = async {
            self.cancel_account_transfers(id);
            let _guard = self.acquire_account_write_guard(id).await?;
            let revision = self
                .storage
                .get_object_storage_account(id)
                .await?
                .map(|account| account.revision)
                .unwrap_or(1);
            self.driver.invalidate_account(id, revision).await?;
            self.clear_account_state(id);
            Ok(())
        }
        .await;
        log_object_storage_error("object_storage_session_close", Some(id), &result);
        result
    }

    pub async fn shutdown(&self) -> Result<()> {
        let result = self.driver.shutdown().await.map_err(DomainError::from);
        log_object_storage_error("object_storage_shutdown", None, &result);
        result
    }

    pub(super) fn account_gate(&self, id: &ObjectStorageAccountId) -> Arc<tokio::sync::RwLock<()>> {
        self.account_gates
            .lock()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
    }

    async fn acquire_account_write_guard(
        &self,
        id: &ObjectStorageAccountId,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>> {
        let acquire = Box::pin(self.account_gate(id).write_owned());
        let timeout = Box::pin(smol::Timer::after(ACCOUNT_WRITE_WAIT_TIMEOUT));
        match select(acquire, timeout).await {
            Either::Left((guard, _)) => Ok(guard),
            Either::Right(_) => Err(DomainError::Other(
                "等待该账号的对象存储操作停止超时，请稍后重试".into(),
            )),
        }
    }

    fn clear_account_state(&self, id: &ObjectStorageAccountId) {
        let prefix = format!("{id}:");
        self.listing_generations
            .lock()
            .retain(|key, _| !key.starts_with(&prefix));
    }

    async fn verify_configured_mounts(
        &self,
        account: &ObjectStorageAccount,
    ) -> Result<AccountVerification> {
        let mounts = mounts::configured_mounts(account)?;
        if mounts.is_empty() {
            return Err(DomainError::InvalidConfig(
                "请至少添加一个 Bucket 挂载".into(),
            ));
        }
        let query = ObjectListQuery::new("", "").map_err(DomainError::InvalidConfig)?;
        for (index, mount) in mounts.iter().enumerate() {
            match self
                .driver
                .list_page(&account.snapshot(), mount, &query, None, index as u64 + 1)
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    let result = verification_from_error(error);
                    if let Ok(AccountVerification::Unverified { reason }) = &result {
                        tracing::warn!(
                            operation = "object_storage_account_verify",
                            account_id = %account.id,
                            mount_id = %mount.id,
                            reason,
                            "object storage account verification incomplete"
                        );
                    }
                    return result;
                }
            }
        }
        Ok(AccountVerification::Verified)
    }

    fn register_transfer(
        &self,
        account_id: &ObjectStorageAccountId,
        cancellation: ramag_domain::entities::TransferCancellation,
    ) -> u64 {
        let id = self.next_transfer_id.fetch_add(1, Ordering::Relaxed);
        self.active_transfers
            .lock()
            .entry(account_id.clone())
            .or_default()
            .insert(id, cancellation);
        id
    }

    fn unregister_transfer(&self, account_id: &ObjectStorageAccountId, transfer_id: u64) {
        let mut transfers = self.active_transfers.lock();
        if let Some(account_transfers) = transfers.get_mut(account_id) {
            account_transfers.remove(&transfer_id);
            if account_transfers.is_empty() {
                transfers.remove(account_id);
            }
        }
    }

    fn cancel_account_transfers(&self, account_id: &ObjectStorageAccountId) {
        if let Some(transfers) = self.active_transfers.lock().get(account_id) {
            for cancellation in transfers.values() {
                cancellation.cancel();
            }
        }
    }
}

pub(super) fn log_object_storage_error<T>(
    operation: &'static str,
    account_id: Option<&ObjectStorageAccountId>,
    result: &Result<T>,
) {
    let Err(error) = result else {
        return;
    };
    let account_id = account_id.map_or_else(|| "-".to_string(), ToString::to_string);
    match error {
        DomainError::ObjectStorage(error)
            if error.category == ObjectStorageErrorCategory::Cancelled =>
        {
            tracing::info!(
                operation,
                account_id = %account_id,
                "object storage operation cancelled"
            );
        }
        DomainError::ObjectStorage(error) => tracing::error!(
            operation,
            error = %error.safe_message,
            account_id = %account_id,
            category = ?error.category,
            provider_operation = error.operation,
            provider_code = error.provider_code.as_deref().unwrap_or("-"),
            request_id = error.request_id.as_deref().unwrap_or("-"),
            retryable = error.retryable,
            "object storage operation failed"
        ),
        error => tracing::warn!(
            operation,
            error = %error,
            account_id = %account_id,
            "object storage operation failed"
        ),
    }
}

fn verification_from_error(error: ObjectStorageError) -> Result<AccountVerification> {
    match error.category {
        ObjectStorageErrorCategory::Network
        | ObjectStorageErrorCategory::Tls
        | ObjectStorageErrorCategory::Timeout
        | ObjectStorageErrorCategory::RateLimited
        | ObjectStorageErrorCategory::Provider => Ok(AccountVerification::Unverified {
            reason: error.safe_message,
        }),
        _ => Err(DomainError::ObjectStorage(error)),
    }
}

fn normalize_manual_mounts(account: &mut ObjectStorageAccount) {
    for bucket in &mut account.manual_buckets {
        if account.provider == ramag_domain::entities::CloudProvider::AliyunOss {
            bucket.region = bucket
                .region
                .strip_prefix("oss-")
                .unwrap_or(&bucket.region)
                .to_string();
        }
    }
}

fn workspace_preference_key(id: &ObjectStorageAccountId) -> String {
    format!("{WORKSPACE_PREFERENCE_PREFIX}{id}")
}
