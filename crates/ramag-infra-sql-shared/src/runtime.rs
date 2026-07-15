//! tokio↔smol 桥接：GPUI 用 smol，sqlx 强依赖 tokio。
//! 全局 tokio 单例 runtime，任务通过 JoinHandle 显式回传业务错误或 panic。

use std::future::Future;

use once_cell::sync::OnceCell;
use tokio::runtime::{Builder, Runtime};

use ramag_domain::error::{DomainError, Result};

static TOKIO_RUNTIME: OnceCell<std::result::Result<Runtime, String>> = OnceCell::new();

/// 惰性初始化全局 tokio runtime；资源不足等构建失败转为领域错误，不终止进程。
pub fn tokio_runtime() -> Result<&'static Runtime> {
    match TOKIO_RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ramag-tokio")
            .enable_all()
            .build()
            .map_err(|error| format!("创建 SQL Tokio runtime 失败：{error}"))
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(DomainError::Other(error.clone())),
    }
}

/// 在 tokio runtime 跑 future；任务 panic / 被取消时转为显式错误。
pub async fn run_in_tokio<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    tokio_runtime()?
        .spawn(future)
        .await
        .map_err(|error| DomainError::Other(format!("SQL Tokio task 异常退出：{error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton() -> Result<()> {
        let r1 = tokio_runtime()?;
        let r2 = tokio_runtime()?;
        assert!(std::ptr::eq(r1, r2));
        Ok(())
    }

    #[test]
    #[allow(clippy::panic)]
    fn task_panic_is_returned_as_error() {
        let result = futures::executor::block_on(run_in_tokio::<_, ()>(async {
            panic!("test panic");
        }));
        assert!(matches!(result, Err(DomainError::Other(message)) if message.contains("panic")));
    }
}
