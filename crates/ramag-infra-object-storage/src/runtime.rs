//! 对象存储专用 Tokio runtime，可显式停止，避免进程退出时遗留任务。

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use ramag_domain::error::{ObjectStorageError, ObjectStorageErrorCategory, ObjectStorageResult};
use tokio::runtime::{Builder, Handle, Runtime};

pub struct RuntimeHost {
    runtime: Mutex<Option<Runtime>>,
    handle: Handle,
    accepting: AtomicBool,
}

impl RuntimeHost {
    pub fn new() -> ObjectStorageResult<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ramag-object-storage")
            .enable_all()
            .build()
            .map_err(|_| runtime_error("创建对象存储运行时失败"))?;
        let handle = runtime.handle().clone();
        Ok(Self {
            runtime: Mutex::new(Some(runtime)),
            handle,
            accepting: AtomicBool::new(true),
        })
    }

    pub async fn run<F, T>(&self, future: F) -> ObjectStorageResult<T>
    where
        F: Future<Output = ObjectStorageResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(runtime_error("对象存储服务已停止"));
        }
        self.handle
            .spawn(future)
            .await
            .map_err(|_| runtime_error("对象存储后台任务异常退出"))?
    }

    pub async fn shutdown(&self) -> ObjectStorageResult<()> {
        self.accepting.store(false, Ordering::Release);
        let Some(runtime) = self.runtime.lock().take() else {
            return Ok(());
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("ramag-object-storage-stop".into())
            .spawn(move || {
                runtime.shutdown_timeout(Duration::from_secs(3));
                let _ = sender.send(());
            })
            .map_err(|_| runtime_error("无法启动对象存储停止线程"))?;
        receiver
            .await
            .map_err(|_| runtime_error("对象存储运行时停止异常"))
    }
}

fn runtime_error(message: &str) -> ObjectStorageError {
    ObjectStorageError::new(ObjectStorageErrorCategory::Provider, "runtime", message)
}
