use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    notification::Notification, spinner::Spinner,
};
use ramag_domain::entities::{TransferProgress, TransferSummary};
use ramag_domain::error::DomainError;
use tracing::{error, info, warn};

mod import_options;

pub use import_options::open_import_options_dialog;

/// 单任务传输状态。
#[derive(Default)]
pub struct TransferState {
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Arc<Mutex<TransferProgress>>>,
}

impl TransferState {
    pub fn active(&self) -> bool {
        self.cancel.is_some()
    }

    /// 开始传输并返回取消位与进度槽。
    pub fn begin(&mut self) -> (Arc<AtomicBool>, Arc<Mutex<TransferProgress>>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(TransferProgress::default()));
        self.cancel = Some(cancel.clone());
        self.progress = Some(progress.clone());
        (cancel, progress)
    }

    pub fn is_current(&self, token: &Arc<AtomicBool>) -> bool {
        self.cancel.as_ref().is_some_and(|c| Arc::ptr_eq(c, token))
    }

    /// 仅当前任务可清理状态。
    pub fn finish(&mut self, token: &Arc<AtomicBool>) -> bool {
        if !self.is_current(token) {
            return false;
        }
        self.cancel = None;
        self.progress = None;
        true
    }

    pub fn request_cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
            info!(
                operation = "database_transfer_cancel",
                "database transfer cancellation requested"
            );
        }
    }

    pub fn cancelling(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }

    /// 非阻塞读取最新进度。
    pub fn progress_line(&self) -> Option<String> {
        let slot = self.progress.as_ref()?;
        match slot.try_lock() {
            Ok(progress) => Some(progress.display_line()),
            Err(_) => None,
        }
    }
}

pub fn progress_sink(
    slot: Arc<Mutex<TransferProgress>>,
) -> impl Fn(TransferProgress) + Send + Sync {
    let poison_reported = AtomicBool::new(false);
    move |progress| match slot.lock() {
        Ok(mut guard) => *guard = progress,
        Err(_) if !poison_reported.swap(true, Ordering::Relaxed) => {
            warn!(
                operation = "database_transfer_progress",
                reason = "lock_poisoned",
                "database transfer progress lock poisoned"
            );
        }
        Err(_) => {}
    }
}

/// 将传输结果转换为通知并记录结构化日志。
pub fn transfer_notification(
    verb: &str,
    cancelled_note: &str,
    outcome: Result<Option<(TransferSummary, String)>, DomainError>,
) -> Option<Notification> {
    match outcome {
        Ok(None) => None,
        Ok(Some((summary, target))) => {
            for warning in &summary.warnings {
                warn!(
                    operation = "database_transfer_warning",
                    verb,
                    target = %target,
                    detail = %warning,
                    "database transfer warning"
                );
            }
            info!(
                operation = "database_transfer_finished",
                verb,
                target = %target,
                objects = summary.objects,
                items = summary.items,
                skipped = summary.skipped,
                failed = summary.failed,
                cancelled = summary.cancelled,
                elapsed_ms = summary.elapsed_ms,
                "database transfer finished"
            );
            let mut text = summary.brief(verb);
            if !summary.warnings.is_empty() || summary.warnings_overflow > 0 {
                text.push_str("；明细见日志");
            }
            Some(if summary.cancelled {
                text.push_str(&format!("；{cancelled_note}"));
                Notification::warning(text).title(target)
            } else if summary.failed > 0 {
                Notification::warning(text).title(target)
            } else {
                Notification::success(text).title(target).autohide(true)
            })
        }
        Err(error) => {
            error!(
                operation = "database_transfer",
                verb,
                error = %error,
                "database transfer failed"
            );
            Some(Notification::error(format!(
                "{verb}失败：{}",
                error.message()
            )))
        }
    }
}

/// 每 120ms 刷新传输进度，任务结束后自动退出。
pub fn spawn_transfer_ticker<V: 'static>(
    cx: &mut Context<V>,
    token: Arc<AtomicBool>,
    is_current: impl Fn(&V, &Arc<AtomicBool>) -> bool + 'static,
) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let still = this
                .update(cx, |view, cx| {
                    let active = is_current(view, &token);
                    if active {
                        cx.notify();
                    }
                    active
                })
                .unwrap_or(false);
            if !still {
                break;
            }
        }
    })
    .detach();
}

/// 渲染传输进度行；空闲时返回 `None`。
pub fn transfer_progress_row<V: 'static>(
    id: &'static str,
    state: &TransferState,
    state_of: impl Fn(&mut V) -> &TransferState + 'static,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    if !state.active() {
        return None;
    }
    let line = state
        .progress_line()
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "准备中…".to_string());
    let cancelling = state.cancelling();
    let theme = cx.theme();
    let (border, muted_fg) = (theme.border, theme.muted_foreground);
    Some(
        h_flex()
            .w_full()
            .items_center()
            .px(px(10.0))
            .py(px(4.0))
            .gap(px(6.0))
            .border_b_1()
            .border_color(border)
            .child(Spinner::new().xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(muted_fg)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(line),
            )
            .child(
                crate::clickable_button(id)
                    .danger()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("取消")
                    .disabled(cancelling)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        state_of(view).request_cancel();
                        cx.notify();
                    })),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_state_tracks_current_task() {
        let mut state = TransferState::default();
        assert!(!state.active());
        let (token, _slot) = state.begin();
        assert!(state.active());
        assert!(state.is_current(&token));

        let stale = Arc::new(AtomicBool::new(false));
        assert!(!state.finish(&stale));
        assert!(state.active());

        state.request_cancel();
        assert!(state.cancelling());
        assert!(state.finish(&token));
        assert!(!state.active());
    }

    #[test]
    fn progress_line_reads_latest_snapshot() {
        let mut state = TransferState::default();
        assert!(state.progress_line().is_none());
        let (_token, slot) = state.begin();
        if let Ok(mut progress) = slot.lock() {
            progress.stage = "导出数据".into();
            progress.object = "users".into();
        }
        let line = state.progress_line().unwrap_or_default();
        assert!(line.contains("users"));
    }
}
