//! 查询标签共享的结果内存预算。

use std::cell::RefCell;
use std::rc::Rc;

use gpui::App;

/// 全局提示线。
pub const GLOBAL_RESULT_WARNING_BYTES: usize = 384 * 1024 * 1024;
/// 全局硬上限。
pub const MAX_GLOBAL_RESULT_BYTES: usize = 512 * 1024 * 1024;

type EvictCallback = Rc<dyn Fn(&mut App) -> bool>;

#[derive(Clone, Default)]
pub struct ResultMemoryBudget {
    state: Rc<RefCell<BudgetState>>,
}

/// 结果面板的预算登记；销毁时自动移除。
pub struct ResultMemoryLease {
    budget: ResultMemoryBudget,
    id: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultMemoryUpdate {
    /// 是否达到提示线。
    pub warning: bool,
    /// 当前结果是否被释放。
    pub current_evicted: bool,
    /// 已释放的非活动结果数。
    pub evicted_results: usize,
    /// 清理后的总占用。
    pub total_bytes: usize,
}

#[derive(Default)]
struct BudgetState {
    entries: Vec<BudgetEntry>,
    next_id: u64,
    clock: u64,
}

struct BudgetEntry {
    id: u64,
    bytes: usize,
    active: bool,
    last_used: u64,
    evict: EvictCallback,
}

impl ResultMemoryBudget {
    pub fn register(&self, evict: impl Fn(&mut App) -> bool + 'static) -> ResultMemoryLease {
        let mut state = self.state.borrow_mut();
        state.next_id = state.next_id.wrapping_add(1).max(1);
        let id = state.next_id;
        let last_used = state.tick();
        state.entries.push(BudgetEntry {
            id,
            bytes: 0,
            active: false,
            last_used,
            evict: Rc::new(evict),
        });
        ResultMemoryLease {
            budget: self.clone(),
            id,
        }
    }
}

impl ResultMemoryLease {
    /// 更新占用，并按 LRU 释放非活动结果。
    pub fn update_bytes(&self, bytes: usize, cx: &mut App) -> ResultMemoryUpdate {
        let (callbacks, outcome) = self.budget.state.borrow_mut().update_entry(self.id, bytes);
        for callback in callbacks {
            let _ = callback(cx);
        }
        outcome
    }

    pub fn set_active(&self, active: bool) {
        let mut state = self.budget.state.borrow_mut();
        let tick = state.tick();
        if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == self.id) {
            entry.active = active;
            if active {
                entry.last_used = tick;
            }
        }
    }

    pub fn touch(&self) {
        let mut state = self.budget.state.borrow_mut();
        let tick = state.tick();
        if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == self.id) {
            entry.last_used = tick;
        }
    }
}

impl Drop for ResultMemoryLease {
    fn drop(&mut self) {
        self.budget
            .state
            .borrow_mut()
            .entries
            .retain(|entry| entry.id != self.id);
    }
}

impl BudgetState {
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1).max(1);
        self.clock
    }

    fn update_entry(&mut self, id: u64, bytes: usize) -> (Vec<EvictCallback>, ResultMemoryUpdate) {
        let tick = self.tick();
        let Some(current_index) = self.entries.iter().position(|entry| entry.id == id) else {
            return (Vec::new(), ResultMemoryUpdate::default());
        };
        self.entries[current_index].bytes = bytes;
        self.entries[current_index].last_used = tick;

        let before_cleanup = self.total_bytes();
        let warning = before_cleanup >= GLOBAL_RESULT_WARNING_BYTES;
        let mut callbacks = Vec::new();
        if before_cleanup > MAX_GLOBAL_RESULT_BYTES {
            let mut candidates: Vec<(u64, usize)> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.id != id && !entry.active && entry.bytes > 0)
                .map(|(index, entry)| (entry.last_used, index))
                .collect();
            candidates.sort_unstable_by_key(|(last_used, _)| *last_used);
            for (_, index) in candidates {
                if self.total_bytes() <= MAX_GLOBAL_RESULT_BYTES {
                    break;
                }
                self.entries[index].bytes = 0;
                callbacks.push(self.entries[index].evict.clone());
            }
        }

        // 无可回收结果时拒绝当前结果，保证硬上限。
        let mut current_evicted = false;
        if self.total_bytes() > MAX_GLOBAL_RESULT_BYTES && self.entries[current_index].bytes > 0 {
            self.entries[current_index].bytes = 0;
            current_evicted = true;
        }

        let total_bytes = self.total_bytes();
        let evicted_results = callbacks.len();
        (
            callbacks,
            ResultMemoryUpdate {
                warning,
                current_evicted,
                evicted_results,
                total_bytes,
            },
        )
    }

    fn total_bytes(&self) -> usize {
        self.entries
            .iter()
            .fold(0usize, |total, entry| total.saturating_add(entry.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback() -> EvictCallback {
        Rc::new(|_| true)
    }

    fn entry(id: u64, bytes: usize, active: bool, last_used: u64) -> BudgetEntry {
        BudgetEntry {
            id,
            bytes,
            active,
            last_used,
            evict: callback(),
        }
    }

    #[test]
    fn hard_limit_evicts_oldest_inactive_result_first() {
        let mut state = BudgetState {
            entries: vec![
                entry(1, 200 * 1024 * 1024, false, 1),
                entry(2, 200 * 1024 * 1024, false, 2),
                entry(3, 0, true, 3),
            ],
            next_id: 3,
            clock: 3,
        };

        let (callbacks, outcome) = state.update_entry(3, 200 * 1024 * 1024);

        assert_eq!(callbacks.len(), 1);
        assert_eq!(state.entries[0].bytes, 0);
        assert_eq!(state.entries[1].bytes, 200 * 1024 * 1024);
        assert!(!outcome.current_evicted);
        assert_eq!(outcome.total_bytes, 400 * 1024 * 1024);
    }

    #[test]
    fn active_result_is_not_an_lru_candidate() {
        let mut state = BudgetState {
            entries: vec![
                entry(1, 256 * 1024 * 1024, true, 1),
                entry(2, 256 * 1024 * 1024, false, 2),
                entry(3, 0, false, 3),
            ],
            next_id: 3,
            clock: 3,
        };

        let (_, outcome) = state.update_entry(3, 1);

        assert_eq!(state.entries[0].bytes, 256 * 1024 * 1024);
        assert_eq!(state.entries[1].bytes, 0);
        assert!(!outcome.current_evicted);
    }

    #[test]
    fn warning_starts_at_exact_global_threshold() {
        let mut below = BudgetState {
            entries: vec![entry(1, 0, true, 1)],
            next_id: 1,
            clock: 1,
        };
        let (_, below_outcome) =
            below.update_entry(1, GLOBAL_RESULT_WARNING_BYTES.saturating_sub(1));
        assert!(!below_outcome.warning);

        let (_, boundary_outcome) = below.update_entry(1, GLOBAL_RESULT_WARNING_BYTES);
        assert!(boundary_outcome.warning);
        assert_eq!(boundary_outcome.total_bytes, GLOBAL_RESULT_WARNING_BYTES);
    }

    #[test]
    fn current_result_is_rejected_when_only_other_active_results_remain() {
        let mut state = BudgetState {
            entries: vec![
                entry(1, MAX_GLOBAL_RESULT_BYTES, true, 1),
                entry(2, 0, true, 2),
            ],
            next_id: 2,
            clock: 2,
        };

        let (callbacks, outcome) = state.update_entry(2, 1);

        assert!(callbacks.is_empty());
        assert!(outcome.current_evicted);
        assert_eq!(outcome.total_bytes, MAX_GLOBAL_RESULT_BYTES);
    }
}
