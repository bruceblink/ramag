#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! COS / OSS 对象存储基础设施适配器。

mod cursor_store;
mod errors;
mod objects;
mod operator_cache;
mod runtime;
mod transport;

use std::sync::Arc;

use async_trait::async_trait;
use ramag_domain::entities::{
    ObjectCapabilities, ObjectDownloadRequest, ObjectListCursor, ObjectListQuery, ObjectMetadata,
    ObjectPage, ObjectStorageAccountId, ObjectStorageAccountSnapshot, ObjectStorageMount,
    ObjectTextPreview, ObjectUploadRequest, is_opendal_safe_key,
};
use ramag_domain::error::ObjectStorageResult;
use ramag_domain::traits::ObjectStorageDriver;

use crate::cursor_store::CursorStore;
use crate::errors::{invalid, map_opendal};
use crate::operator_cache::OperatorCache;
use crate::runtime::RuntimeHost;

pub struct ObjectStorageInfra {
    runtime: Arc<RuntimeHost>,
    operators: Arc<OperatorCache>,
    cursors: Arc<CursorStore>,
}

impl ObjectStorageInfra {
    pub fn new() -> ObjectStorageResult<Self> {
        Ok(Self {
            runtime: Arc::new(RuntimeHost::new()?),
            operators: Arc::new(OperatorCache::default()),
            cursors: Arc::new(CursorStore::default()),
        })
    }
}

#[async_trait]
impl ObjectStorageDriver for ObjectStorageInfra {
    async fn capabilities(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities> {
        self.operators.capabilities(account, mount)
    }

    async fn list_page(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        query: &ObjectListQuery,
        cursor: Option<&ObjectListCursor>,
        request_generation: u64,
    ) -> ObjectStorageResult<ObjectPage> {
        let operator = self.operators.get(account, mount)?;
        let cursors = self.cursors.clone();
        let account = account.clone();
        let mount = mount.clone();
        let query = query.clone();
        let cursor = cursor.cloned();
        self.runtime
            .run(async move {
                cursors
                    .list_page(
                        operator,
                        &account,
                        &mount,
                        &query,
                        cursor.as_ref(),
                        request_generation,
                    )
                    .await
            })
            .await
    }

    async fn stat(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectMetadata> {
        let operator = self.operators.get(account, mount)?;
        let key = key.to_string();
        self.runtime
            .run(async move { objects::stat(operator, &key).await })
            .await
    }

    async fn read_text_preview(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectTextPreview> {
        let operator = self.operators.get(account, mount)?;
        let key = key.to_string();
        self.runtime
            .run(async move { objects::read_text_preview(operator, &key).await })
            .await
    }

    async fn upload(&self, request: ObjectUploadRequest) -> ObjectStorageResult<()> {
        let operator = self.operators.get(&request.account, &request.mount)?;
        self.runtime
            .run(async move { objects::upload(operator, request).await })
            .await
    }

    async fn download(&self, request: ObjectDownloadRequest) -> ObjectStorageResult<()> {
        let operator = self.operators.get(&request.account, &request.mount)?;
        self.runtime
            .run(async move { objects::download(operator, request).await })
            .await
    }

    async fn delete(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<()> {
        if account.read_only {
            return Err(invalid("delete", "生产模式下不能删除对象"));
        }
        if !is_opendal_safe_key(key) {
            return Err(invalid("delete", "当前对象键无法由 OpenDAL 安全表示"));
        }
        let operator = self.operators.get(account, mount)?;
        let key = key.to_string();
        self.runtime
            .run(async move {
                operator
                    .delete(&key)
                    .await
                    .map_err(|error| map_opendal("delete", error))
            })
            .await
    }

    async fn invalidate_account(
        &self,
        account_id: &ObjectStorageAccountId,
        minimum_revision: u64,
    ) -> ObjectStorageResult<()> {
        self.operators.invalidate(account_id, minimum_revision);
        self.cursors.invalidate_account(&account_id.to_string());
        Ok(())
    }

    async fn shutdown(&self) -> ObjectStorageResult<()> {
        self.cursors.clear();
        self.operators.clear();
        self.runtime.shutdown().await
    }
}
