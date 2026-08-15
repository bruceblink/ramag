//! 对象存储上传、下载请求与进度回调。

use std::path::PathBuf;
use std::sync::Arc;

use crate::entities::ssh::{OverwritePolicy, TransferCancellation};

use super::{ObjectStorageAccountSnapshot, ObjectStorageMount};

#[derive(Clone)]
pub struct ObjectTransferProgress {
    pub transferred: u64,
    pub total: u64,
}

pub type ObjectProgressFn = Arc<dyn Fn(ObjectTransferProgress) + Send + Sync>;

#[derive(Clone)]
pub struct ObjectUploadRequest {
    pub account: ObjectStorageAccountSnapshot,
    pub mount: ObjectStorageMount,
    pub key: String,
    pub local_path: PathBuf,
    pub overwrite: OverwritePolicy,
    pub cancellation: TransferCancellation,
    pub progress: ObjectProgressFn,
}

#[derive(Clone)]
pub struct ObjectDownloadRequest {
    pub account: ObjectStorageAccountSnapshot,
    pub mount: ObjectStorageMount,
    pub key: String,
    pub local_path: PathBuf,
    pub overwrite: OverwritePolicy,
    pub cancellation: TransferCancellation,
    pub progress: ObjectProgressFn,
}
