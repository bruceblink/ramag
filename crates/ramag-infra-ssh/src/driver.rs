//! OpenSSH 驱动实例与会话管理器生命周期。

use super::*;

pub struct OpenSshDriver {
    pub(super) locator: OpenSshLocator,
    pub(super) sessions: SessionCache,
    pub(super) transfers: TransferEngine,
    pub(super) askpass: Arc<askpass::AskPassBroker>,
    pub(super) diagnostic_global: Arc<tokio::sync::Semaphore>,
    pub(super) diagnostic_profiles:
        parking_lot::Mutex<HashMap<SshProfileId, Arc<tokio::sync::Semaphore>>>,
}

impl OpenSshDriver {
    pub fn new() -> Self {
        let askpass = Arc::new(askpass::AskPassBroker::new());
        Self {
            locator: OpenSshLocator::default(),
            sessions: SessionCache::new(askpass.clone()),
            transfers: TransferEngine::default(),
            askpass,
            diagnostic_global: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DIAGNOSTICS)),
            diagnostic_profiles: parking_lot::Mutex::new(HashMap::new()),
        }
    }
}

impl Default for OpenSshDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OpenSshDriver {
    fn drop(&mut self) {
        let sessions = self.sessions.clone();
        if let Ok(runtime) = tokio_runtime() {
            runtime.spawn(async move {
                sessions.shutdown().await;
            });
        }
    }
}
