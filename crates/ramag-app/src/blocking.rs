//! CPU 与文件 I/O 共用的固定工作线程池，避免阻塞 GPUI 前台执行器或频繁创建线程。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use futures::channel::oneshot;

use ramag_domain::error::{DomainError, Result};

type Job = Box<dyn FnOnce() + Send + 'static>;

struct WorkerPool {
    sender: mpsc::SyncSender<Job>,
}

impl WorkerPool {
    fn new() -> std::result::Result<Self, String> {
        let workers =
            std::thread::available_parallelism().map_or(2, |count| count.get().clamp(2, 4));
        Self::with_limits(workers, workers * 16)
    }

    fn with_limits(workers: usize, queue_capacity: usize) -> std::result::Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));

        for index in 0..workers {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("ramag-app-worker-{index}"))
                .spawn(move || worker_loop(receiver))
                .map_err(|error| format!("启动 app worker 失败：{error}"))?;
        }
        Ok(Self { sender })
    }

    fn execute(&self, job: Job) -> Result<()> {
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                Err(DomainError::Other("后台任务队列繁忙，请稍后重试".into()))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(DomainError::Other("后台任务线程池已停止".into()))
            }
        }
    }
}

fn worker_loop(receiver: Arc<Mutex<mpsc::Receiver<Job>>>) {
    loop {
        let job = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => {
                tracing::warn!("app worker queue lock poisoned");
                return;
            }
        };
        match job {
            Ok(job) => job(),
            Err(_) => return,
        }
    }
}

fn pool() -> Result<&'static WorkerPool> {
    static POOL: OnceLock<std::result::Result<WorkerPool, String>> = OnceLock::new();
    match POOL.get_or_init(WorkerPool::new) {
        Ok(pool) => Ok(pool),
        Err(error) => Err(DomainError::Other(error.clone())),
    }
}

/// 在线程数受控的共享线程池中执行 CPU 或文件 I/O。
pub async fn run_blocking<F, T>(operation: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    pool()?.execute(Box::new(move || {
        let result = catch_unwind(AssertUnwindSafe(operation))
            .unwrap_or_else(|_| Err(DomainError::Other("后台任务执行时发生 panic".into())));
        let _ = sender.send(result);
    }))?;
    receiver
        .await
        .unwrap_or_else(|_| Err(DomainError::Other("后台任务异常退出".into())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use futures::future::join_all;

    use super::*;

    #[test]
    fn many_jobs_reuse_a_bounded_set_of_workers() -> Result<()> {
        futures::executor::block_on(async {
            let jobs = (0..32).map(|_| {
                run_blocking(|| {
                    Ok(std::thread::current()
                        .name()
                        .unwrap_or("unnamed")
                        .to_string())
                })
            });
            let names: HashSet<String> = join_all(jobs)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .collect();

            assert!(!names.is_empty());
            assert!(names.len() <= 4);
            assert!(
                names
                    .iter()
                    .all(|name| name.starts_with("ramag-app-worker-"))
            );
            Ok(())
        })
    }

    #[test]
    #[allow(clippy::panic)]
    fn panicking_job_returns_error_without_losing_pool() {
        futures::executor::block_on(async {
            let error = run_blocking::<_, ()>(|| panic!("test panic")).await;
            assert!(matches!(error, Err(DomainError::Other(message)) if message.contains("panic")));
            assert!(matches!(run_blocking(|| Ok(42)).await, Ok(42)));
        });
    }

    #[test]
    fn full_queue_is_rejected_without_blocking() -> Result<()> {
        let pool = WorkerPool::with_limits(1, 1).map_err(DomainError::Other)?;
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        pool.execute(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        }))?;
        started_rx
            .recv()
            .map_err(|error| DomainError::Other(error.to_string()))?;
        pool.execute(Box::new(move || {
            let _ = done_tx.send(());
        }))?;

        let overloaded = pool.execute(Box::new(|| {}));
        assert!(matches!(
            overloaded,
            Err(DomainError::Other(message)) if message.contains("繁忙")
        ));

        release_tx
            .send(())
            .map_err(|error| DomainError::Other(error.to_string()))?;
        done_rx
            .recv()
            .map_err(|error| DomainError::Other(error.to_string()))?;
        Ok(())
    }
}
