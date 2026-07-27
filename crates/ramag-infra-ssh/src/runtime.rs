//! SSH 专用 Tokio runtime，避免终端与文件传输阻塞其他驱动。

use std::future::Future;

use once_cell::sync::OnceCell;
use tokio::runtime::{Builder, Runtime};

use ramag_domain::error::{DomainError, Result};

static TOKIO_RUNTIME: OnceCell<std::result::Result<Runtime, String>> = OnceCell::new();

pub fn tokio_runtime() -> Result<&'static Runtime> {
    match TOKIO_RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(3)
            .thread_name("ramag-ssh-tokio")
            .enable_all()
            .build()
            .map_err(|error| format!("创建 SSH Tokio runtime 失败：{error}"))
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
    tokio_runtime()?
        .spawn(future)
        .await
        .map_err(|error| DomainError::Other(format!("SSH Tokio task 异常退出：{error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_singleton() -> Result<()> {
        assert!(std::ptr::eq(tokio_runtime()?, tokio_runtime()?));
        Ok(())
    }

    #[test]
    fn bridge_returns_value() {
        let value = futures::executor::block_on(run_in_tokio(async { Ok(42) }));
        assert!(matches!(value, Ok(42)));
    }
}
