//! SSH 用例编排：配置、远程文件操作、传输状态与工作区恢复。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ramag_domain::entities::{
    MAX_QUEUED_TRANSFERS, MAX_REMOTE_FILE_PREVIEW_BYTES, MAX_TRANSFER_HISTORY, OverwritePolicy,
    RemoteDirectory, RemoteEntryKind, RemoteFileChunk, RemoteFileChunkPosition, RemoteFilePreview,
    SshCapability, SshLaunchCommand, SshProfile, SshProfileId, SshWorkspacePreference,
    TransferCancellation, TransferDirection, TransferId, TransferStatus, TransferTask,
    validate_local_transfer_path, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{SshDriver, Storage};

mod helpers;

use helpers::{
    bounded_error, cancel_tasks, ensure_sftp_writable, normalized_workspace_preference,
    parse_workspace_preference,
};

const WORKSPACE_PREFERENCE_KEY: &str = "ssh_workspaces_v1";
const MAX_WORKSPACE_PREFERENCE_BYTES: usize = 64 * 1024;
const MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES: usize = MAX_WORKSPACE_PREFERENCE_BYTES * 2 + 1024;
const ENCRYPTED_WORKSPACE_PREFIX: &str = "enc-v1:";
const MAX_TRANSFER_ERROR_BYTES: usize = 16 * 1024;
const TRANSFER_STOP_GRACE: Duration = Duration::from_secs(5);
const TRANSFER_STOP_POLL: Duration = Duration::from_millis(25);

struct TransferState {
    tasks: VecDeque<TransferTask>,
    cancellations: HashMap<TransferId, TransferCancellation>,
}

impl TransferState {
    fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            cancellations: HashMap::new(),
        }
    }

    fn prune_history(&mut self) {
        let mut finished = self
            .tasks
            .iter()
            .filter(|task| task.status.is_terminal())
            .count();
        let mut index = 0usize;
        while finished > MAX_TRANSFER_HISTORY && index < self.tasks.len() {
            if self.tasks[index].status.is_terminal() {
                self.tasks.remove(index);
                finished -= 1;
            } else {
                index += 1;
            }
        }
    }
}

struct TransferStore {
    state: Mutex<TransferState>,
    revision: AtomicU64,
}

impl TransferStore {
    fn new() -> Self {
        Self {
            state: Mutex::new(TransferState::new()),
            revision: AtomicU64::new(0),
        }
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn enqueue(&self, task: TransferTask) -> Result<TransferId> {
        let mut state = self.state.lock();
        let queued = state
            .tasks
            .iter()
            .filter(|existing| !existing.status.is_terminal())
            .count();
        if queued >= MAX_QUEUED_TRANSFERS {
            return Err(DomainError::Other(format!(
                "等待或进行中的传输已达 {MAX_QUEUED_TRANSFERS} 个上限"
            )));
        }
        state.prune_history();
        let id = task.id.clone();
        state
            .cancellations
            .insert(id.clone(), TransferCancellation::default());
        state.tasks.push_back(task);
        drop(state);
        self.changed();
        Ok(id)
    }

    fn begin(&self, id: &TransferId) -> Result<(TransferTask, TransferCancellation)> {
        let mut state = self.state.lock();
        let task = state
            .tasks
            .iter_mut()
            .find(|task| &task.id == id)
            .ok_or_else(|| DomainError::NotFound(format!("传输任务 {id}")))?;
        if task.status != TransferStatus::Waiting {
            return Err(DomainError::Other(format!(
                "传输任务 {id} 当前状态不可执行：{:?}",
                task.status
            )));
        }
        task.mark_running();
        let snapshot = task.clone();
        let cancellation = state
            .cancellations
            .get(id)
            .cloned()
            .ok_or_else(|| DomainError::Other(format!("传输任务 {id} 缺少取消句柄")))?;
        drop(state);
        self.changed();
        Ok((snapshot, cancellation))
    }

    fn progress(&self, id: &TransferId, transferred: u64, total: u64) {
        let mut state = self.state.lock();
        if let Some(task) = state.tasks.iter_mut().find(|task| &task.id == id) {
            task.update_progress(transferred, total);
        }
        drop(state);
        self.changed();
    }

    fn finish(&self, id: &TransferId, result: &Result<()>, cancelled: bool) {
        let mut state = self.state.lock();
        if let Some(task) = state.tasks.iter_mut().find(|task| &task.id == id) {
            let result = result
                .as_ref()
                .map(|_| ())
                .map_err(|error| bounded_error(error.to_string()));
            task.finish(result, cancelled);
        }
        state.cancellations.remove(id);
        state.prune_history();
        drop(state);
        self.changed();
    }
}

pub struct SshService {
    driver: Arc<dyn SshDriver>,
    storage: Arc<dyn Storage>,
    transfers: Arc<TransferStore>,
}

impl SshService {
    pub fn new(driver: Arc<dyn SshDriver>, storage: Arc<dyn Storage>) -> Self {
        Self {
            driver,
            storage,
            transfers: Arc::new(TransferStore::new()),
        }
    }

    pub async fn list_profiles(&self) -> Result<Vec<SshProfile>> {
        self.storage.list_ssh_profiles().await
    }

    pub async fn get_profile(&self, id: &SshProfileId) -> Result<Option<SshProfile>> {
        self.storage.get_ssh_profile(id).await
    }

    pub async fn save_profile(&self, profile: &SshProfile) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        self.storage.save_ssh_profile(profile).await?;
        self.cancel_profile_transfers(&profile.id);
        self.wait_for_profile_transfers(&profile.id).await;
        if let Err(error) = self.driver.disconnect(&profile.id).await {
            tracing::warn!(
                error = %error,
                profile_id = %profile.id,
                "disconnect stale ssh profile session failed"
            );
        }
        Ok(())
    }

    pub async fn delete_profile(&self, id: &SshProfileId) -> Result<()> {
        self.storage.delete_ssh_profile(id).await?;
        self.cancel_profile_transfers(id);
        self.wait_for_profile_transfers(id).await;
        if let Err(error) = self.driver.disconnect(id).await {
            tracing::warn!(error = %error, profile_id = %id, "disconnect deleted ssh profile failed");
        }
        match self.load_workspace_preference().await {
            Ok(mut preference) => {
                preference
                    .workspaces
                    .retain(|workspace| &workspace.profile_id != id);
                preference
                    .path_favorites
                    .retain(|favorite| &favorite.profile_id != id);
                if preference.active_profile_id.as_ref() == Some(id) {
                    preference.active_profile_id = None;
                }
                if let Err(error) = self.save_workspace_preference(&preference).await {
                    tracing::warn!(error = %error, profile_id = %id, "cleanup ssh workspace preference failed");
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, profile_id = %id, "load ssh workspace preference for cleanup failed");
            }
        }
        Ok(())
    }

    pub async fn probe(&self, custom_path: Option<&str>) -> Result<SshCapability> {
        self.driver.probe(custom_path).await
    }

    pub async fn test_connection(&self, profile: &SshProfile) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        self.driver.test_connection(profile).await
    }

    pub async fn terminal_command(&self, profile: &SshProfile) -> Result<SshLaunchCommand> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        self.driver.terminal_command(profile).await
    }

    pub async fn report_terminal_launch_failure(&self, executable: &str) {
        self.driver.report_terminal_launch_failure(executable).await;
    }

    pub async fn list_directory(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteDirectory> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        self.driver.list_directory(profile, path).await
    }

    pub async fn read_file_preview(
        &self,
        profile: &SshProfile,
        path: &str,
    ) -> Result<RemoteFilePreview> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        self.driver.read_file_preview(profile, path).await
    }

    pub async fn read_file_chunk(
        &self,
        profile: &SshProfile,
        path: &str,
        position: RemoteFileChunkPosition,
    ) -> Result<RemoteFileChunk> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        self.driver.read_file_chunk(profile, path, position).await
    }

    pub async fn save_file(
        &self,
        profile: &SshProfile,
        path: &str,
        expected: &[u8],
        contents: &[u8],
    ) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        ensure_sftp_writable(profile)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        if expected.len() > MAX_REMOTE_FILE_PREVIEW_BYTES
            || contents.len() > MAX_REMOTE_FILE_PREVIEW_BYTES
        {
            return Err(DomainError::InvalidConfig(format!(
                "编辑文件不能超过 {} MiB",
                MAX_REMOTE_FILE_PREVIEW_BYTES / 1024 / 1024
            )));
        }
        self.driver
            .save_file(profile, path, expected, contents)
            .await
    }

    pub async fn create_directory(&self, profile: &SshProfile, path: &str) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        ensure_sftp_writable(profile)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        self.driver.create_directory(profile, path).await
    }

    pub async fn rename(&self, profile: &SshProfile, old_path: &str, new_path: &str) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        ensure_sftp_writable(profile)?;
        validate_remote_path(old_path).map_err(DomainError::InvalidConfig)?;
        validate_remote_path(new_path).map_err(DomainError::InvalidConfig)?;
        self.driver.rename(profile, old_path, new_path).await
    }

    pub async fn remove(
        &self,
        profile: &SshProfile,
        path: &str,
        kind: RemoteEntryKind,
    ) -> Result<()> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        ensure_sftp_writable(profile)?;
        validate_remote_path(path).map_err(DomainError::InvalidConfig)?;
        self.driver.remove(profile, path, kind).await
    }

    pub fn enqueue_upload(
        &self,
        profile: &SshProfile,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<TransferId> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        ensure_sftp_writable(profile)?;
        validate_local_transfer_path(local_path).map_err(DomainError::InvalidConfig)?;
        validate_remote_path(remote_path).map_err(DomainError::InvalidConfig)?;
        let local_path = local_path
            .to_str()
            .ok_or_else(|| DomainError::InvalidConfig("本地路径不是 UTF-8".into()))?;
        self.transfers.enqueue(TransferTask::new(
            profile.id.clone(),
            TransferDirection::Upload,
            local_path,
            remote_path,
        ))
    }

    pub fn enqueue_download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<TransferId> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_local_transfer_path(local_path).map_err(DomainError::InvalidConfig)?;
        validate_remote_path(remote_path).map_err(DomainError::InvalidConfig)?;
        let local_path = local_path
            .to_str()
            .ok_or_else(|| DomainError::InvalidConfig("本地路径不是 UTF-8".into()))?;
        self.transfers.enqueue(TransferTask::new(
            profile.id.clone(),
            TransferDirection::Download,
            local_path,
            remote_path,
        ))
    }

    pub fn enqueue_directory_download(
        &self,
        profile: &SshProfile,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<TransferId> {
        profile.validate().map_err(DomainError::InvalidConfig)?;
        validate_local_transfer_path(local_path).map_err(DomainError::InvalidConfig)?;
        validate_remote_path(remote_path).map_err(DomainError::InvalidConfig)?;
        let local_path = local_path
            .to_str()
            .ok_or_else(|| DomainError::InvalidConfig("本地路径不是 UTF-8".into()))?;
        self.transfers.enqueue(TransferTask::new(
            profile.id.clone(),
            TransferDirection::DownloadArchive,
            local_path,
            remote_path,
        ))
    }

    pub async fn execute_transfer(
        &self,
        id: &TransferId,
        profile: &SshProfile,
        overwrite: OverwritePolicy,
    ) -> Result<()> {
        let (task, cancellation) = self.transfers.begin(id)?;
        tracing::info!(
            task_id = %id,
            profile_id = %profile.id,
            direction = ?task.direction,
            "ssh transfer started"
        );
        if task.profile_id != profile.id {
            let error = DomainError::InvalidConfig("传输任务与 SSH 配置不匹配".into());
            self.transfers.finish(
                id,
                &Err(DomainError::InvalidConfig(error.message().into())),
                false,
            );
            return Err(error);
        }
        if task.direction == TransferDirection::Upload
            && let Err(error) = ensure_sftp_writable(profile)
        {
            self.transfers.finish(
                id,
                &Err(DomainError::Forbidden(error.message().into())),
                false,
            );
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
                        profile,
                        &local_path,
                        &task.remote_path,
                        overwrite,
                        cancellation.clone(),
                        progress,
                    )
                    .await
            }
            TransferDirection::Download => {
                self.driver
                    .download(
                        profile,
                        &task.remote_path,
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
                        profile,
                        &task.remote_path,
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
                tracing::info!(task_id = %id, profile_id = %profile.id, "ssh transfer finished")
            }
            Err(error) => tracing::warn!(
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
        self.driver.disconnect(profile_id).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.cancel_all_transfers();
        self.wait_for_all_transfers().await;
        self.driver.shutdown().await
    }

    pub async fn load_workspace_preference(&self) -> Result<SshWorkspacePreference> {
        let Some(stored) = self
            .storage
            .get_preference(WORKSPACE_PREFERENCE_KEY)
            .await?
        else {
            return Ok(SshWorkspacePreference::default());
        };
        if stored.len() > MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES {
            return Err(DomainError::InvalidConfig("SSH 工作区恢复数据过大".into()));
        }
        let json = if let Some(encoded) = stored.strip_prefix(ENCRYPTED_WORKSPACE_PREFIX) {
            let encrypted = hex::decode(encoded).map_err(|error| {
                DomainError::Storage(format!("SSH 工作区密文编码无效：{error}"))
            })?;
            let plain = self.storage.unseal(&encrypted).await?;
            String::from_utf8(plain).map_err(|error| {
                DomainError::Storage(format!("SSH 工作区解密结果不是 UTF-8：{error}"))
            })?
        } else {
            // 兼容短期开发版本写入的明文偏好；下次保存会自动迁移为密文。
            stored
        };
        parse_workspace_preference(&json)
    }

    pub async fn save_workspace_preference(
        &self,
        preference: &SshWorkspacePreference,
    ) -> Result<()> {
        let preference = normalized_workspace_preference(preference.clone())?;
        let json = serde_json::to_string(&preference)
            .map_err(|error| DomainError::Storage(format!("序列化 SSH 工作区失败：{error}")))?;
        if json.len() > MAX_WORKSPACE_PREFERENCE_BYTES {
            return Err(DomainError::InvalidConfig("SSH 工作区恢复数据过大".into()));
        }
        let encrypted = self.storage.seal(json.as_bytes()).await?;
        let stored = format!("{ENCRYPTED_WORKSPACE_PREFIX}{}", hex::encode(encrypted));
        if stored.len() > MAX_ENCRYPTED_WORKSPACE_PREFERENCE_BYTES {
            return Err(DomainError::InvalidConfig(
                "加密后的 SSH 工作区恢复数据过大".into(),
            ));
        }
        self.storage
            .set_preference(WORKSPACE_PREFERENCE_KEY, &stored)
            .await
    }

    fn cancel_profile_transfers(&self, profile_id: &SshProfileId) {
        let mut state = self.transfers.state.lock();
        let matching_ids: Vec<TransferId> = state
            .tasks
            .iter()
            .filter(|task| &task.profile_id == profile_id && !task.status.is_terminal())
            .map(|task| task.id.clone())
            .collect();
        cancel_tasks(&mut state, &matching_ids);
        drop(state);
        self.transfers.changed();
    }

    fn cancel_all_transfers(&self) {
        let mut state = self.transfers.state.lock();
        let matching_ids: Vec<TransferId> = state
            .tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .map(|task| task.id.clone())
            .collect();
        cancel_tasks(&mut state, &matching_ids);
        drop(state);
        self.transfers.changed();
    }

    async fn wait_for_profile_transfers(&self, profile_id: &SshProfileId) {
        let deadline = Instant::now() + TRANSFER_STOP_GRACE;
        loop {
            let active = self
                .transfers
                .state
                .lock()
                .tasks
                .iter()
                .any(|task| &task.profile_id == profile_id && !task.status.is_terminal());
            if !active {
                return;
            }
            if Instant::now() >= deadline {
                tracing::warn!(profile_id = %profile_id, "ssh transfers did not stop before disconnect deadline");
                return;
            }
            smol::Timer::after(TRANSFER_STOP_POLL).await;
        }
    }

    async fn wait_for_all_transfers(&self) {
        let deadline = Instant::now() + TRANSFER_STOP_GRACE;
        loop {
            let active = self
                .transfers
                .state
                .lock()
                .tasks
                .iter()
                .any(|task| !task.status.is_terminal());
            if !active {
                return;
            }
            if Instant::now() >= deadline {
                tracing::warn!("ssh transfers did not stop before shutdown deadline");
                return;
            }
            smol::Timer::after(TRANSFER_STOP_POLL).await;
        }
    }
}

#[cfg(test)]
mod tests;
