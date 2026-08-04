//! 应用级同步独占门禁：状态和视觉遮罩共用同一权威来源。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use ramag_domain::entities::{DataSyncProgress, DataSyncStage, DataSyncSummary, DataSyncTaskId};

const MAX_SYNC_ERROR_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSyncExecutionContext {
    pub source_connection: String,
    pub source_scope: String,
    pub target_connection: String,
    pub target_scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSyncGatePhase {
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl DataSyncGatePhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSyncGateSnapshot {
    pub task_id: DataSyncTaskId,
    pub phase: DataSyncGatePhase,
    pub context: DataSyncExecutionContext,
    pub progress: DataSyncProgress,
    pub summary: Option<DataSyncSummary>,
    pub error: Option<String>,
}

/// 只有持有当前代次许可的执行器可以更新或结束任务。
#[derive(Debug, Clone)]
pub struct DataSyncPermit {
    generation: u64,
    task_id: DataSyncTaskId,
    cancel: Arc<AtomicBool>,
}

impl DataSyncPermit {
    pub fn task_id(&self) -> &DataSyncTaskId {
        &self.task_id
    }

    pub fn cancellation_requested(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

#[derive(Debug)]
struct ActiveTask {
    generation: u64,
    task_id: DataSyncTaskId,
    phase: DataSyncGatePhase,
    context: DataSyncExecutionContext,
    progress: DataSyncProgress,
    summary: Option<DataSyncSummary>,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
    started_at: Instant,
}

#[derive(Debug, Default)]
struct GateInner {
    generation: u64,
    active: Option<ActiveTask>,
}

/// 全应用共享一个实例。终态仍视为阻塞，只有用户确认结果后才能释放。
#[derive(Debug, Default)]
pub struct DataSyncGate {
    inner: Mutex<GateInner>,
}

impl DataSyncGate {
    pub fn begin(
        &self,
        task_id: DataSyncTaskId,
        context: DataSyncExecutionContext,
    ) -> Option<DataSyncPermit> {
        let mut inner = self.inner.lock();
        if inner.active.is_some() {
            return None;
        }
        inner.generation = inner.generation.wrapping_add(1);
        let generation = inner.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        inner.active = Some(ActiveTask {
            generation,
            task_id: task_id.clone(),
            phase: DataSyncGatePhase::Running,
            context,
            progress: DataSyncProgress::default(),
            summary: None,
            error: None,
            cancel: cancel.clone(),
            started_at: Instant::now(),
        });
        Some(DataSyncPermit {
            generation,
            task_id,
            cancel,
        })
    }

    pub fn is_blocking(&self) -> bool {
        self.inner.lock().active.is_some()
    }

    pub fn snapshot(&self) -> Option<DataSyncGateSnapshot> {
        self.inner.lock().active.as_ref().map(|active| {
            let mut progress = active.progress.clone();
            progress.elapsed_ms = progress.elapsed_ms.max(
                active
                    .started_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            );
            DataSyncGateSnapshot {
                task_id: active.task_id.clone(),
                phase: active.phase,
                context: active.context.clone(),
                progress,
                summary: active.summary.clone(),
                error: active.error.clone(),
            }
        })
    }

    /// 高频调用方应先在执行器侧节流；终态和旧代次更新会被拒绝。
    pub fn update_progress(&self, permit: &DataSyncPermit, progress: DataSyncProgress) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = current_task_mut(&mut inner, permit) else {
            return false;
        };
        if active.phase.terminal() {
            return false;
        }
        active.progress = progress;
        if active.phase == DataSyncGatePhase::Cancelling {
            active.progress.stage = DataSyncStage::Cancelling;
        }
        true
    }

    /// 请求协作式取消。占屏和逻辑门禁继续保持，直到执行器进入终态且用户确认。
    pub fn request_cancel(&self, permit: &DataSyncPermit) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = current_task_mut(&mut inner, permit) else {
            return false;
        };
        if active.phase != DataSyncGatePhase::Running {
            return false;
        }
        active.cancel.store(true, Ordering::Release);
        active.phase = DataSyncGatePhase::Cancelling;
        active.progress.stage = DataSyncStage::Cancelling;
        true
    }

    /// UI 占屏层只持任务 ID；任务 ID 不匹配或已进入终态时拒绝，防止旧界面误取消新任务。
    pub fn request_cancel_current(&self, task_id: &DataSyncTaskId) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = inner.active.as_mut() else {
            return false;
        };
        if &active.task_id != task_id || active.phase != DataSyncGatePhase::Running {
            return false;
        }
        active.cancel.store(true, Ordering::Release);
        active.phase = DataSyncGatePhase::Cancelling;
        active.progress.stage = DataSyncStage::Cancelling;
        true
    }

    pub fn finish_completed(&self, permit: &DataSyncPermit, summary: DataSyncSummary) -> bool {
        self.finish(permit, DataSyncGatePhase::Completed, summary, None)
    }

    pub fn finish_cancelled(&self, permit: &DataSyncPermit, mut summary: DataSyncSummary) -> bool {
        summary.cancelled = true;
        self.finish(permit, DataSyncGatePhase::Cancelled, summary, None)
    }

    pub fn finish_failed(
        &self,
        permit: &DataSyncPermit,
        summary: DataSyncSummary,
        error: impl Into<String>,
    ) -> bool {
        let error = compact_text(error.into(), MAX_SYNC_ERROR_BYTES);
        self.finish(permit, DataSyncGatePhase::Failed, summary, Some(error))
    }

    /// 仅结果终态可以确认关闭；Running / Cancelling 不能被 UI 或快捷键提前释放。
    pub fn acknowledge_and_release(&self, permit: &DataSyncPermit) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = current_task(&inner, permit) else {
            return false;
        };
        if !active.phase.terminal() {
            return false;
        }
        inner.active = None;
        true
    }

    /// 结果占屏层按当前任务 ID 确认关闭；运行中、取消中和旧任务 ID 均不能释放门禁。
    pub fn acknowledge_current(&self, task_id: &DataSyncTaskId) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = inner.active.as_ref() else {
            return false;
        };
        if &active.task_id != task_id || !active.phase.terminal() {
            return false;
        }
        inner.active = None;
        true
    }

    fn finish(
        &self,
        permit: &DataSyncPermit,
        phase: DataSyncGatePhase,
        summary: DataSyncSummary,
        error: Option<String>,
    ) -> bool {
        let mut inner = self.inner.lock();
        let Some(active) = current_task_mut(&mut inner, permit) else {
            return false;
        };
        if active.phase.terminal() {
            return false;
        }
        active.phase = phase;
        active.progress.scanned = active.progress.scanned.max(summary.scanned);
        active.progress.inserted = active.progress.inserted.max(summary.inserted);
        active.progress.skipped = active.progress.skipped.max(summary.skipped);
        active.progress.failed = active.progress.failed.max(summary.failed);
        active.progress.bytes = active.progress.bytes.max(summary.bytes);
        active.progress.warnings = active
            .progress
            .warnings
            .max((summary.warnings.len() as u64).saturating_add(summary.warnings_overflow));
        active.progress.elapsed_ms = active.progress.elapsed_ms.max(summary.elapsed_ms);
        active.summary = Some(summary);
        active.error = error;
        true
    }
}

fn current_task<'a>(inner: &'a GateInner, permit: &DataSyncPermit) -> Option<&'a ActiveTask> {
    inner
        .active
        .as_ref()
        .filter(|active| active.generation == permit.generation && active.task_id == permit.task_id)
}

fn current_task_mut<'a>(
    inner: &'a mut GateInner,
    permit: &DataSyncPermit,
) -> Option<&'a mut ActiveTask> {
    inner
        .active
        .as_mut()
        .filter(|active| active.generation == permit.generation && active.task_id == permit.task_id)
}

fn compact_text(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    text.truncate(boundary);
    text.push('…');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(name: &str) -> DataSyncExecutionContext {
        DataSyncExecutionContext {
            source_connection: format!("source-{name}"),
            source_scope: "source-db".into(),
            target_connection: format!("target-{name}"),
            target_scope: "target-db".into(),
        }
    }

    #[test]
    fn gate_is_exclusive_until_terminal_result_is_acknowledged() {
        let gate = DataSyncGate::default();
        let permit = gate
            .begin(DataSyncTaskId::new(), context("first"))
            .expect("空闲门禁应允许开始");
        assert!(gate.is_blocking());
        assert!(
            gate.begin(DataSyncTaskId::new(), context("second"))
                .is_none()
        );
        assert!(!gate.acknowledge_and_release(&permit));

        assert!(gate.finish_completed(&permit, DataSyncSummary::default()));
        assert!(gate.is_blocking());
        assert!(gate.acknowledge_and_release(&permit));
        assert!(!gate.is_blocking());
    }

    #[test]
    fn cancellation_remains_blocking_until_safe_finish_and_acknowledgement() {
        let gate = DataSyncGate::default();
        let permit = gate
            .begin(DataSyncTaskId::new(), context("cancel"))
            .expect("空闲门禁应允许开始");
        assert!(!permit.cancellation_requested());
        assert!(gate.request_cancel(&permit));
        assert!(permit.cancellation_requested());
        assert_eq!(
            gate.snapshot().map(|snapshot| snapshot.phase),
            Some(DataSyncGatePhase::Cancelling)
        );
        assert!(!gate.acknowledge_and_release(&permit));

        assert!(gate.finish_cancelled(&permit, DataSyncSummary::default()));
        let snapshot = gate.snapshot().expect("取消结果应保持占屏");
        assert_eq!(snapshot.phase, DataSyncGatePhase::Cancelled);
        assert!(snapshot.summary.is_some_and(|summary| summary.cancelled));
        assert!(gate.acknowledge_and_release(&permit));
    }

    #[test]
    fn overlay_actions_require_current_task_id_and_terminal_result() {
        let gate = DataSyncGate::default();
        let task_id = DataSyncTaskId::new();
        let stale_id = DataSyncTaskId::new();
        let permit = gate
            .begin(task_id.clone(), context("overlay"))
            .expect("开始应成功");
        assert!(!gate.request_cancel_current(&stale_id));
        assert!(gate.request_cancel_current(&task_id));
        assert!(!gate.acknowledge_current(&task_id));
        assert!(gate.finish_cancelled(&permit, DataSyncSummary::default()));
        assert!(!gate.acknowledge_current(&stale_id));
        assert!(gate.acknowledge_current(&task_id));
    }

    #[test]
    fn stale_permit_cannot_update_finish_or_release_new_task() {
        let gate = DataSyncGate::default();
        let stale = gate
            .begin(DataSyncTaskId::new(), context("stale"))
            .expect("第一次应成功");
        assert!(gate.finish_completed(&stale, DataSyncSummary::default()));
        assert!(gate.acknowledge_and_release(&stale));

        let current = gate
            .begin(DataSyncTaskId::new(), context("current"))
            .expect("释放后应允许新任务");
        assert!(!gate.update_progress(&stale, DataSyncProgress::default()));
        assert!(!gate.finish_completed(&stale, DataSyncSummary::default()));
        assert!(!gate.acknowledge_and_release(&stale));
        assert_eq!(
            gate.snapshot().map(|snapshot| snapshot.task_id),
            Some(current.task_id().clone())
        );
    }

    #[test]
    fn cancelling_progress_cannot_restore_non_cancelling_stage() {
        let gate = DataSyncGate::default();
        let permit = gate
            .begin(DataSyncTaskId::new(), context("progress"))
            .expect("开始应成功");
        assert!(gate.request_cancel(&permit));
        assert!(gate.update_progress(
            &permit,
            DataSyncProgress {
                stage: DataSyncStage::Writing,
                inserted: 10,
                ..DataSyncProgress::default()
            }
        ));
        let snapshot = gate.snapshot().expect("任务应存在");
        assert_eq!(snapshot.phase, DataSyncGatePhase::Cancelling);
        assert_eq!(snapshot.progress.stage, DataSyncStage::Cancelling);
        assert_eq!(snapshot.progress.inserted, 10);
    }

    #[test]
    fn terminal_state_rejects_late_progress_and_duplicate_finish() {
        let gate = DataSyncGate::default();
        let permit = gate
            .begin(DataSyncTaskId::new(), context("late"))
            .expect("开始应成功");
        let mut summary = DataSyncSummary {
            elapsed_ms: 123,
            ..DataSyncSummary::default()
        };
        summary.push_warning("target changed");
        assert!(gate.finish_failed(&permit, summary, "write failed"));
        assert!(!gate.update_progress(
            &permit,
            DataSyncProgress {
                inserted: 999,
                ..DataSyncProgress::default()
            }
        ));
        assert!(!gate.finish_completed(&permit, DataSyncSummary::default()));
        let snapshot = gate.snapshot().expect("失败结果应存在");
        assert_eq!(snapshot.progress.inserted, 0);
        assert_eq!(snapshot.progress.warnings, 1);
        assert!(snapshot.progress.elapsed_ms >= 123);
        assert_eq!(snapshot.error.as_deref(), Some("write failed"));
    }

    #[test]
    fn error_text_is_bounded_without_breaking_utf8() {
        let gate = DataSyncGate::default();
        let permit = gate
            .begin(DataSyncTaskId::new(), context("error"))
            .expect("开始应成功");
        assert!(gate.finish_failed(
            &permit,
            DataSyncSummary::default(),
            "错".repeat(MAX_SYNC_ERROR_BYTES)
        ));
        let error = gate
            .snapshot()
            .and_then(|snapshot| snapshot.error)
            .expect("错误文本应存在");
        assert!(error.len() <= MAX_SYNC_ERROR_BYTES + '…'.len_utf8());
        assert!(error.ends_with('…'));
    }
}
