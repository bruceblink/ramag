//! SSH 用例编排：配置、远程文件操作、传输状态与工作区恢复。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ramag_domain::entities::{
    DiagnosticCancellation, MAX_CONCURRENT_DIAGNOSTICS, MAX_CONCURRENT_DIAGNOSTICS_PER_PROFILE,
    MAX_QUEUED_TRANSFERS, MAX_REMOTE_FILE_PREVIEW_BYTES, MAX_TRANSFER_HISTORY, OverwritePolicy,
    RemoteCapabilityState, RemoteDirectory, RemoteEntryKind, RemoteFileChunk,
    RemoteFileChunkPosition, RemoteFilePreview, RemoteOperatingSystem, RemotePath,
    RemotePlatformPreference, RemoteShellKind, SftpNamespaceKind, SshCapability,
    SshDiagnosticOperation, SshDiagnosticResult, SshLaunchCommand, SshProfile, SshProfileId,
    SshRemoteCapabilities, SshWorkspacePreference, TransferCancellation, TransferDirection,
    TransferId, TransferStatus, TransferTask, infer_sftp_namespace, validate_local_transfer_path,
    validate_remote_name_for_namespace, validate_remote_path,
};
use ramag_domain::error::{DomainError, Result};
use ramag_domain::traits::{JumpServerDriver, SshDriver, Storage};

mod helpers;
mod jumpserver;
mod remote;
mod transfer_ops;

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
    jumpserver_driver: Option<Arc<dyn JumpServerDriver>>,
    storage: Arc<dyn Storage>,
    transfers: Arc<TransferStore>,
    terminal_policy: Mutex<TerminalPolicyState>,
    remote_capabilities: Mutex<HashMap<SshProfileId, CachedRemoteCapabilities>>,
    diagnostic_global: Arc<tokio::sync::Semaphore>,
    diagnostic_profiles: Mutex<HashMap<SshProfileId, Arc<tokio::sync::Semaphore>>>,
}

#[derive(Clone)]
struct CachedRemoteCapabilities {
    profile: SshProfile,
    capabilities: SshRemoteCapabilities,
}

#[derive(Default)]
struct TerminalPolicyState {
    blocked: HashSet<SshProfileId>,
    generations: HashMap<SshProfileId, u64>,
}

impl SshService {
    pub fn new(driver: Arc<dyn SshDriver>, storage: Arc<dyn Storage>) -> Self {
        Self {
            driver,
            jumpserver_driver: None,
            storage,
            transfers: Arc::new(TransferStore::new()),
            terminal_policy: Mutex::new(TerminalPolicyState::default()),
            remote_capabilities: Mutex::new(HashMap::new()),
            diagnostic_global: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DIAGNOSTICS)),
            diagnostic_profiles: Mutex::new(HashMap::new()),
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
        self.advance_terminal_generation(&profile.id);
        self.remote_capabilities.lock().remove(&profile.id);
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
        self.block_terminal_launches(id);
        self.remote_capabilities.lock().remove(id);
        self.diagnostic_profiles.lock().remove(id);
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

fn advance_generation(policy: &mut TerminalPolicyState, profile_id: &SshProfileId) {
    let generation = policy.generations.entry(profile_id.clone()).or_default();
    *generation = generation.wrapping_add(1);
}

#[cfg(test)]
mod tests;
