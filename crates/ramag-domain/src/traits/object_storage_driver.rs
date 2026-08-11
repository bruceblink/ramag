//! 云对象存储控制面与数据面端口。

use async_trait::async_trait;

use crate::entities::{
    ObjectCapabilities, ObjectDownloadRequest, ObjectListCursor, ObjectListQuery, ObjectMetadata,
    ObjectPage, ObjectStorageAccountId, ObjectStorageAccountSnapshot, ObjectStorageMount,
    ObjectTextPreview, ObjectUploadRequest,
};
use crate::error::ObjectStorageResult;

#[async_trait]
pub trait ObjectStorageDriver: Send + Sync {
    async fn capabilities(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
    ) -> ObjectStorageResult<ObjectCapabilities>;

    async fn list_page(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        query: &ObjectListQuery,
        cursor: Option<&ObjectListCursor>,
        request_generation: u64,
    ) -> ObjectStorageResult<ObjectPage>;

    async fn stat(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectMetadata>;

    async fn read_text_preview(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<ObjectTextPreview>;

    async fn upload(&self, request: ObjectUploadRequest) -> ObjectStorageResult<()>;

    async fn download(&self, request: ObjectDownloadRequest) -> ObjectStorageResult<()>;

    async fn delete(
        &self,
        account: &ObjectStorageAccountSnapshot,
        mount: &ObjectStorageMount,
        key: &str,
    ) -> ObjectStorageResult<()>;

    async fn invalidate_account(
        &self,
        account_id: &ObjectStorageAccountId,
        minimum_revision: u64,
    ) -> ObjectStorageResult<()>;

    async fn shutdown(&self) -> ObjectStorageResult<()>;
}
