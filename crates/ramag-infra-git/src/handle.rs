//! 已打开仓库句柄 + 写操作串行化锁

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramag_domain::error::Result;

use crate::runtime::run_blocking;

/// 已打开仓库的稳定路径与写锁；读查询不共享仓库锁，可由系统 Git 并发执行。
pub(crate) struct OpenRepo {
    pub(crate) path: PathBuf,
    /// linked worktree 的 Git 状态目录不一定是 `<path>/.git`，打开时固定解析一次。
    pub(crate) git_dir: PathBuf,
    /// 写操作串行化锁，避免并发触发 `.git/index.lock` 冲突
    pub(crate) write_lock: Arc<parking_lot::Mutex<()>>,
    /// History 连续分页复用一个 `git log` 流；查询变化或仓库关闭时自动终止。
    pub(crate) log_pager: crate::log::LogPagerSlot,
}

/// 写操作 helper：worker 线程内先 lock 再跑。所有写 git index 的方法走这个
pub(crate) async fn run_write_blocking<F, T>(handle: Arc<OpenRepo>, f: F) -> Result<T>
where
    F: FnOnce(&Path) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    run_blocking(move || {
        let _g = handle.write_lock.lock();
        f(&handle.path)
    })
    .await
}
