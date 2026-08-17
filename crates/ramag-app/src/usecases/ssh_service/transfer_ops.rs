//! SSH / SFTP 传输任务编排。

use super::remote::{
    ensure_remote_write_platform, profile_for_capabilities, resolved_new_remote_path,
    resolved_remote_path,
};
use super::*;

impl SshService {
    pub fn enqueue_upload(
        &self,
        profile: &SshProfile,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<TransferId> {
        self.enqueue_transfer(profile, TransferDirection::Upload, local_path, remote_path)
    }

    pub fn enqueue_download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<TransferId> {
        self.enqueue_transfer(
            profile,
            TransferDirection::Download,
            local_path,
            remote_path,
        )
    }

    pub fn enqueue_directory_download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<TransferId> {
        self.enqueue_transfer(
            profile,
            TransferDirection::DownloadArchive,
            local_path,
            remote_path,
        )
    }

    fn enqueue_transfer(
        &self,
        profile: &SshProfile,
        direction: TransferDirection,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<TransferId> {
        let result = (|| {
            profile.validate().map_err(DomainError::InvalidConfig)?;
            if direction == TransferDirection::Upload {
                ensure_sftp_writable(profile)?;
            }
            validate_local_transfer_path(local_path).map_err(DomainError::InvalidConfig)?;
            validate_remote_path(remote_path).map_err(DomainError::InvalidConfig)?;
            let local_path = local_path
                .to_str()
                .ok_or_else(|| DomainError::InvalidConfig("本地路径不是 UTF-8".into()))?;
            self.transfers.enqueue(TransferTask::new(
                profile.id.clone(),
                direction,
                local_path,
                remote_path,
            ))
        })();
        match &result {
            Ok(task_id) => tracing::debug!(
                operation = "ssh_transfer_enqueue",
                task_id = %task_id,
                profile_id = %profile.id,
                direction = ?direction,
                "ssh transfer queued"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_transfer_enqueue",
                error = %error,
                profile_id = %profile.id,
                direction = ?direction,
                "queue ssh transfer failed"
            ),
        }
        result
    }

    pub async fn execute_transfer(
        &self,
        id: &TransferId,
        overwrite: OverwritePolicy,
    ) -> Result<()> {
        let (task, cancellation) = match self.transfers.begin(id) {
            Ok(task) => task,
            Err(error) => {
                tracing::warn!(
                    operation = "ssh_transfer",
                    error = %error,
                    task_id = %id,
                    stage = "begin",
                    "start ssh transfer failed"
                );
                return Err(error);
            }
        };
        let profile = match self.current_profile(&task.profile_id).await {
            Ok(profile) => profile,
            Err(error) => {
                self.finish_transfer_preflight_failure(id, &task, "profile", &error);
                return Err(error);
            }
        };
        let capabilities = match self.capabilities_for_profile(&profile, false).await {
            Ok(capabilities) => capabilities,
            Err(error) => {
                self.finish_transfer_preflight_failure(id, &task, "capabilities", &error);
                return Err(error);
            }
        };
        let effective_profile = profile_for_capabilities(&profile, &capabilities);
        let remote_path = match task.direction {
            TransferDirection::Upload => resolved_new_remote_path(&capabilities, &task.remote_path),
            TransferDirection::Download | TransferDirection::DownloadArchive => {
                resolved_remote_path(&capabilities, &task.remote_path)
            }
        };
        let remote_path = match remote_path {
            Ok(path) => path,
            Err(error) => {
                self.finish_transfer_preflight_failure(id, &task, "remote_path", &error);
                return Err(error);
            }
        };
        tracing::info!(
            operation = "ssh_transfer",
            task_id = %id,
            profile_id = %profile.id,
            direction = ?task.direction,
            "ssh transfer started"
        );
        if task.direction == TransferDirection::Upload
            && let Err(error) = ensure_sftp_writable(&profile)
        {
            self.finish_transfer_preflight_failure(id, &task, "write_permission", &error);
            return Err(error);
        }
        if task.direction == TransferDirection::Upload
            && let Err(error) = ensure_remote_write_platform(&profile, &capabilities)
        {
            self.finish_transfer_preflight_failure(id, &task, "remote_platform", &error);
            return Err(error);
        }
        let transfer_store = self.transfers.clone();
        let progress_id = id.clone();
        let progress = Arc::new(move |transferred, total| {
            transfer_store.progress(&progress_id, transferred, total);
        });
        let local_path = PathBuf::from(&task.local_path);
        let result = match task.direction {
            TransferDirection::Upload => {
                self.driver
                    .upload(
                        &effective_profile,
                        &local_path,
                        &remote_path,
                        overwrite,
                        cancellation.clone(),
                        progress,
                    )
                    .await
            }
            TransferDirection::Download => {
                self.driver
                    .download(
                        &effective_profile,
                        &remote_path,
                        &local_path,
                        overwrite,
                        cancellation.clone(),
                        progress,
                    )
                    .await
            }
            TransferDirection::DownloadArchive => {
                self.driver
                    .download_directory(
                        &effective_profile,
                        &remote_path,
                        &local_path,
                        overwrite,
                        cancellation.clone(),
                        progress,
                    )
                    .await
            }
        };
        self.transfers
            .finish(id, &result, cancellation.is_cancelled());
        match &result {
            Ok(()) => {
                tracing::info!(
                    operation = "ssh_transfer",
                    task_id = %id,
                    profile_id = %profile.id,
                    "ssh transfer finished"
                )
            }
            Err(error) => tracing::warn!(
                operation = "ssh_transfer",
                error = %error,
                task_id = %id,
                profile_id = %profile.id,
                "ssh transfer failed"
            ),
        }
        result
    }

    pub fn cancel_transfer(&self, id: &TransferId) -> bool {
        let mut state = self.transfers.state.lock();
        let Some(cancellation) = state.cancellations.get(id).cloned() else {
            return false;
        };
        let waiting = state
            .tasks
            .iter()
            .any(|task| &task.id == id && task.status == TransferStatus::Waiting);
        if waiting {
            if let Some(task) = state.tasks.iter_mut().find(|task| &task.id == id) {
                task.finish(Err("传输已取消".into()), true);
            }
            state.cancellations.remove(id);
            state.prune_history();
        } else {
            cancellation.cancel();
        }
        drop(state);
        self.transfers.changed();
        true
    }

    pub fn retry_transfer(&self, id: &TransferId) -> Result<TransferId> {
        let result = (|| {
            let state = self.transfers.state.lock();
            let task = state
                .tasks
                .iter()
                .find(|task| &task.id == id)
                .cloned()
                .ok_or_else(|| DomainError::NotFound(format!("传输任务 {id}")))?;
            if !task.status.is_terminal() {
                return Err(DomainError::Other("只能重试已结束的传输任务".into()));
            }
            drop(state);
            self.transfers.enqueue(TransferTask::new(
                task.profile_id,
                task.direction,
                task.local_path,
                task.remote_path,
            ))
        })();
        match &result {
            Ok(retry_id) => tracing::debug!(
                operation = "ssh_transfer_retry",
                task_id = %id,
                retry_task_id = %retry_id,
                "ssh transfer retry queued"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_transfer_retry",
                error = %error,
                task_id = %id,
                "retry ssh transfer failed"
            ),
        }
        result
    }

    pub fn transfer_tasks(&self) -> Vec<TransferTask> {
        self.transfers.state.lock().tasks.iter().cloned().collect()
    }

    pub fn transfer_revision(&self) -> u64 {
        self.transfers.revision.load(Ordering::Acquire)
    }

    pub fn clear_finished_transfers(&self) {
        let mut state = self.transfers.state.lock();
        state.tasks.retain(|task| !task.status.is_terminal());
        drop(state);
        self.transfers.changed();
    }

    pub async fn disconnect(&self, profile_id: &SshProfileId) -> Result<()> {
        self.cancel_profile_transfers(profile_id);
        self.wait_for_profile_transfers(profile_id).await;
        let result = self.driver.disconnect(profile_id).await;
        match &result {
            Ok(()) => tracing::info!(
                operation = "ssh_profile_disconnect",
                profile_id = %profile_id,
                "ssh profile disconnected"
            ),
            Err(error) => tracing::warn!(
                operation = "ssh_profile_disconnect",
                error = %error,
                profile_id = %profile_id,
                "disconnect ssh profile failed"
            ),
        }
        result
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.cancel_all_transfers();
        self.wait_for_all_transfers().await;
        let result = self.driver.shutdown().await;
        match &result {
            Ok(()) => tracing::info!(operation = "ssh_shutdown", "ssh service shut down"),
            Err(error) => tracing::warn!(
                operation = "ssh_shutdown",
                error = %error,
                "shut down ssh service failed"
            ),
        }
        result
    }

    fn finish_transfer_preflight_failure(
        &self,
        id: &TransferId,
        task: &TransferTask,
        stage: &'static str,
        error: &DomainError,
    ) {
        let failure = Err(DomainError::Other(error.message().into()));
        self.transfers.finish(id, &failure, false);
        tracing::warn!(
            operation = "ssh_transfer",
            error = %error,
            task_id = %id,
            profile_id = %task.profile_id,
            direction = ?task.direction,
            stage,
            "prepare ssh transfer failed"
        );
    }
}
