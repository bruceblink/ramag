//! 异步写操作串行化闸门：防重复提交，并用代次隔离重置前的旧回包。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationToken(u64);

#[derive(Debug, Default)]
pub struct AsyncMutationGate {
    generation: u64,
    busy: bool,
}

impl AsyncMutationGate {
    /// 空闲时开始一次操作；忙碌时返回 None，调用方应给用户明确反馈。
    pub fn begin(&mut self) -> Option<MutationToken> {
        if self.busy {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.busy = true;
        Some(MutationToken(self.generation))
    }

    /// 仅当前代次可以结束闸门；旧任务回包不能解锁更新的操作。
    pub fn finish(&mut self, token: MutationToken) -> bool {
        if !self.busy || token.0 != self.generation {
            return false;
        }
        self.busy = false;
        true
    }

    /// 上下文切换时使在途 token 失效，并允许新上下文立即发起操作。
    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.busy = false;
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }
}

#[cfg(test)]
mod tests {
    use super::AsyncMutationGate;

    #[test]
    fn overlapping_mutations_are_rejected_until_current_finishes() {
        let mut gate = AsyncMutationGate::default();
        let token = gate.begin();
        assert!(token.is_some());
        let Some(token) = token else {
            return;
        };

        assert!(gate.is_busy());
        assert!(gate.begin().is_none());
        assert!(gate.finish(token));
        assert!(!gate.is_busy());
        assert!(gate.begin().is_some());
    }

    #[test]
    fn stale_completion_cannot_unlock_new_mutation() {
        let mut gate = AsyncMutationGate::default();
        let stale = gate.begin();
        assert!(stale.is_some());
        let Some(stale) = stale else {
            return;
        };
        gate.reset();
        let current = gate.begin();
        assert!(current.is_some());
        let Some(current) = current else {
            return;
        };

        assert!(!gate.finish(stale));
        assert!(gate.is_busy());
        assert!(gate.finish(current));
        assert!(!gate.is_busy());
    }
}
