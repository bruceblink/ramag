//! 更新检查专用 Tokio runtime，隔离 HTTP 与磁盘流式 I/O。

use std::future::Future;
use std::sync::OnceLock;

use ramag_domain::error::{DomainError, Result};
use tokio::runtime::{Builder, Runtime};

fn runtime() -> Result<&'static Runtime> {
    static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ramag-update-tokio")
            .enable_all()
            .build()
            .map_err(|error| format!("创建更新检查 runtime 失败：{error}"))
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(DomainError::Other(error.clone())),
    }
}

pub async fn run_in_tokio<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    runtime()?
        .spawn(future)
        .await
        .map_err(|error| DomainError::Other(format!("更新检查任务异常退出：{error}")))?
}
