//! 图片删除撤销窗口的内存状态机。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use ramag_domain::entities::ClipId;

#[derive(Default)]
pub(super) struct PendingMediaDeletes {
    next_token: AtomicU64,
    items: Mutex<HashMap<ClipId, PendingMediaDelete>>,
}

struct PendingMediaDelete {
    token: u64,
    paths: Vec<String>,
}

impl PendingMediaDeletes {
    /// 返回本次删除的唯一代际，避免“撤销后再次删除”被上一次的旧计时器提前清理。
    pub(super) fn stage(&self, id: ClipId, paths: Vec<String>) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed) + 1;
        self.items
            .lock()
            .insert(id, PendingMediaDelete { token, paths });
        token
    }

    pub(super) fn take_for_restore(&self, id: &ClipId) -> Option<(u64, Vec<String>)> {
        self.items
            .lock()
            .remove(id)
            .map(|pending| (pending.token, pending.paths))
    }

    /// 回存失败时恢复原代际，使已在运行的原计时器仍能按期清理。
    pub(super) fn put_back(&self, id: ClipId, token: u64, paths: Vec<String>) {
        self.items
            .lock()
            .entry(id)
            .or_insert(PendingMediaDelete { token, paths });
    }

    pub(super) fn expire(&self, id: &ClipId, token: u64) -> Option<Vec<String>> {
        let mut items = self.items.lock();
        if items.get(id).is_none_or(|pending| pending.token != token) {
            return None;
        }
        items.remove(id).map(|pending| pending.paths)
    }
}
