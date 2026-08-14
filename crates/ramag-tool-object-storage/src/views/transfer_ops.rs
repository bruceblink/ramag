use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{Context, Window};
use ramag_domain::entities::{
    ObjectStorageAccountId, ObjectStorageMountId, OverwritePolicy, TransferCancellation,
};
use ramag_domain::error::{DomainError, ObjectStorageErrorCategory};

use super::model::{
    ObjectStorageView, ObjectTransferDirection, ObjectTransferStatus, PendingTransferConflict,
    TransferHistoryUi, TransferUi,
};

impl ObjectStorageView {
    pub(super) fn run_upload(
        &mut self,
        path: std::path::PathBuf,
        key: String,
        overwrite: OverwritePolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        if self.transfer_active(
            &account_id,
            &mount.id,
            &key,
            ObjectTransferDirection::Upload,
        ) {
            self.transfers_visible = true;
            cx.notify();
            return;
        }
        let service = self.service.clone();
        let local_path = path.display().to_string();
        let local_path_for_log = local_path.clone();
        let retry_path = path.clone();
        let retry_key = key.clone();
        let cancellation = TransferCancellation::default();
        let transferred = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let transfer_id = self.next_transfer_id;
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1).max(1);
        self.transfers.push(TransferUi {
            id: transfer_id,
            account_id: account_id.clone(),
            mount_id: mount.id.clone(),
            key: key.clone(),
            label: key.clone(),
            local_path,
            direction: ObjectTransferDirection::Upload,
            cancellation: cancellation.clone(),
            transferred: transferred.clone(),
            total: total.clone(),
        });
        self.transfers_visible = true;
        self.show_detail = false;
        self.persist_workspace(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = service
                .upload_object(
                    &account_id,
                    &mount,
                    key,
                    path,
                    overwrite,
                    cancellation,
                    Arc::new(move |progress| {
                        transferred.store(progress.transferred, Ordering::Relaxed);
                        total.store(progress.total, Ordering::Relaxed);
                    }),
                )
                .await;
            let existing = match &result {
                Err(error) if is_conflict(error) && overwrite == OverwritePolicy::Refuse => {
                    match service.stat_object(&account_id, &mount, &retry_key).await {
                        Ok(metadata) => Some(metadata),
                        Err(metadata_error) => {
                            tracing::warn!(
                                operation = "object_storage_upload_conflict_stat",
                                account_id = %account_id,
                                mount_id = %mount.id,
                                bucket = %mount.bucket,
                                key = %retry_key,
                                error = %metadata_error,
                                "load conflicting object metadata failed"
                            );
                            None
                        }
                    }
                }
                _ => None,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let (status, history_error) = transfer_result(&result);
                this.finish_transfer(transfer_id, status, history_error);
                match result {
                    Ok(()) => {
                        this.notice = Some(("上传完成".into(), false));
                        this.load_first_page(window, cx);
                    }
                    Err(error) if is_conflict(&error) && overwrite == OverwritePolicy::Refuse => {
                        if this.pending_upload.is_none() {
                            let existing_summary = existing
                                .map(|metadata| {
                                    format!(
                                        "现有大小：{} B；最后修改：{}",
                                        metadata.size,
                                        metadata
                                            .last_modified
                                            .map(|value| value.to_rfc3339())
                                            .unwrap_or_else(|| "未知".into())
                                    )
                                })
                                .unwrap_or_else(|| "现有对象元数据不可读".into());
                            this.pending_upload = Some(PendingTransferConflict {
                                path: retry_path,
                                key: retry_key,
                                existing_summary,
                            });
                            this.request_overwrite_upload(window, cx);
                        } else {
                            this.error(
                                "多个上传目标同时冲突；请先处理当前覆盖确认，再重试其余上传",
                            );
                        }
                    }
                    Err(error) if is_cancelled(&error) => {}
                    Err(error) => {
                        tracing::error!(
                            operation = "object_storage_upload",
                            account_id = %account_id,
                            mount_id = %mount.id,
                            bucket = %mount.bucket,
                            key = %retry_key,
                            local_path = %local_path_for_log,
                            error = %error,
                            "upload object failed"
                        );
                        this.error(format!("上传失败：{}", error.user_message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn run_download(
        &mut self,
        path: std::path::PathBuf,
        key: String,
        overwrite: OverwritePolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.clone(),
            self.selected_mount.clone(),
        ) else {
            return;
        };
        if self.transfer_active(
            &account_id,
            &mount.id,
            &key,
            ObjectTransferDirection::Download,
        ) {
            self.transfers_visible = true;
            self.show_detail = false;
            cx.notify();
            return;
        }
        let service = self.service.clone();
        let local_path = path.display().to_string();
        let local_path_for_log = local_path.clone();
        let retry_path = path.clone();
        let retry_key = key.clone();
        let cancellation = TransferCancellation::default();
        let transferred = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let transfer_id = self.next_transfer_id;
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1).max(1);
        self.transfers.push(TransferUi {
            id: transfer_id,
            account_id: account_id.clone(),
            mount_id: mount.id.clone(),
            key: key.clone(),
            label: key.clone(),
            local_path,
            direction: ObjectTransferDirection::Download,
            cancellation: cancellation.clone(),
            transferred: transferred.clone(),
            total: total.clone(),
        });
        self.transfers_visible = true;
        self.show_detail = false;
        self.persist_workspace(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = service
                .download_object(
                    &account_id,
                    &mount,
                    key,
                    path,
                    overwrite,
                    cancellation,
                    Arc::new(move |progress| {
                        transferred.store(progress.transferred, Ordering::Relaxed);
                        total.store(progress.total, Ordering::Relaxed);
                    }),
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let (status, history_error) = transfer_result(&result);
                this.finish_transfer(transfer_id, status, history_error);
                match result {
                    Ok(()) => this.notice = Some(("下载完成".into(), false)),
                    Err(error) if is_conflict(&error) && overwrite == OverwritePolicy::Refuse => {
                        if this.pending_download.is_none() {
                            this.pending_download = Some(PendingTransferConflict {
                                path: retry_path,
                                key: retry_key,
                                existing_summary: "本地目标文件已存在".into(),
                            });
                            this.request_overwrite_download(window, cx);
                        } else {
                            this.error(
                                "多个下载目标同时冲突；请先处理当前覆盖确认，再重试其余下载",
                            );
                        }
                    }
                    Err(error) if is_cancelled(&error) => {}
                    Err(error) => {
                        tracing::error!(
                            operation = "object_storage_download",
                            account_id = %account_id,
                            mount_id = %mount.id,
                            bucket = %mount.bucket,
                            key = %retry_key,
                            local_path = %local_path_for_log,
                            error = %error,
                            "download object failed"
                        );
                        this.error(format!("下载失败：{}", error.user_message()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn download_active_for_key(&self, key: &str) -> bool {
        let (Some(account_id), Some(mount)) = (
            self.selected_account_id.as_ref(),
            self.selected_mount.as_ref(),
        ) else {
            return false;
        };
        self.transfer_active(
            account_id,
            &mount.id,
            key,
            ObjectTransferDirection::Download,
        )
    }

    fn transfer_active(
        &self,
        account_id: &ObjectStorageAccountId,
        mount_id: &ObjectStorageMountId,
        key: &str,
        direction: ObjectTransferDirection,
    ) -> bool {
        self.transfers.iter().any(|transfer| {
            &transfer.account_id == account_id
                && &transfer.mount_id == mount_id
                && transfer.key == key
                && transfer.direction == direction
        })
    }

    fn finish_transfer(
        &mut self,
        transfer_id: u64,
        status: ObjectTransferStatus,
        error: Option<String>,
    ) {
        let Some(index) = self
            .transfers
            .iter()
            .position(|transfer| transfer.id == transfer_id)
        else {
            return;
        };
        let transfer = self.transfers.remove(index);
        self.transfer_history.retain(|record| {
            record.account_id != transfer.account_id
                || record.mount_id != transfer.mount_id
                || record.key != transfer.key
                || record.direction != transfer.direction
        });
        self.transfer_history.push_front(TransferHistoryUi {
            account_id: transfer.account_id,
            mount_id: transfer.mount_id,
            key: transfer.key,
            label: transfer.label,
            local_path: transfer.local_path,
            direction: transfer.direction,
            status,
            error,
        });
        self.transfer_history
            .truncate(ramag_domain::entities::MAX_OBJECT_STORAGE_TRANSFER_HISTORY);
    }
}

fn transfer_result(
    result: &ramag_domain::error::Result<()>,
) -> (ObjectTransferStatus, Option<String>) {
    match result {
        Ok(()) => (ObjectTransferStatus::Completed, None),
        Err(error) if is_cancelled(error) => (ObjectTransferStatus::Cancelled, None),
        Err(error) => (ObjectTransferStatus::Failed, Some(error.user_message())),
    }
}

fn is_conflict(error: &DomainError) -> bool {
    matches!(
        error,
        DomainError::ObjectStorage(error)
            if error.category == ObjectStorageErrorCategory::Conflict
    )
}

fn is_cancelled(error: &DomainError) -> bool {
    matches!(
        error,
        DomainError::ObjectStorage(error)
            if error.category == ObjectStorageErrorCategory::Cancelled
    )
}

#[cfg(test)]
mod tests {
    use ramag_domain::error::ObjectStorageError;

    use super::*;

    #[test]
    fn cancelled_transfer_is_not_reported_as_failure() {
        let result = Err(DomainError::ObjectStorage(ObjectStorageError::new(
            ObjectStorageErrorCategory::Cancelled,
            "download",
            "操作已取消",
        )));

        let (status, message) = transfer_result(&result);

        assert!(matches!(status, ObjectTransferStatus::Cancelled));
        assert!(message.is_none());
    }
}
