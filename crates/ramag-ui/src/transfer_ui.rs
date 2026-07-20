//! 按库导出 / 导入的共享 UI 件：传输状态槽（取消位 + 进度快照）、
//! 进度轮询 ticker、导入冲突策略选择对话框。三个数据库工具共用

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, Window,
    div, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _, h_flex, notification::Notification, spinner::Spinner, v_flex,
};
use ramag_domain::entities::{ConflictPolicy, TransferProgress, TransferSummary};
use ramag_domain::error::DomainError;
use tracing::{info, warn};

/// 面板持有的传输状态：一次只允许一个进行中的导出 / 导入
#[derive(Default)]
pub struct TransferState {
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Arc<Mutex<TransferProgress>>>,
}

impl TransferState {
    pub fn active(&self) -> bool {
        self.cancel.is_some()
    }

    /// 开始一次传输，返回（取消位, 进度槽）。取消位同时充当任务代次 token：
    /// 迟到的完成回调用 [`Self::finish`] 校验归属
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

    /// 归属校验通过则清槽并返回 true；迟到回调返回 false（不得再改面板状态）
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
        }
    }

    pub fn cancelling(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }

    /// 最新进度行；渲染期用 try_lock 避免阻塞
    pub fn progress_line(&self) -> Option<String> {
        let slot = self.progress.as_ref()?;
        match slot.try_lock() {
            Ok(progress) => Some(progress.display_line()),
            Err(_) => None,
        }
    }
}

/// 服务层进度回调：把最新快照写进共享槽（渲染侧 try_lock 读取）
pub fn progress_sink(
    slot: Arc<Mutex<TransferProgress>>,
) -> impl Fn(TransferProgress) + Send + Sync {
    move |progress| {
        if let Ok(mut guard) = slot.lock() {
            *guard = progress;
        }
    }
}

/// 传输完成通知：None = 用户取消了文件选择（静默）。
/// 汇总进 toast，警告明细进日志；`cancelled_note` 区分导出（文件未生成）与导入（部分已生效）
pub fn transfer_notification(
    verb: &str,
    cancelled_note: &str,
    outcome: Result<Option<(TransferSummary, String)>, DomainError>,
) -> Option<Notification> {
    match outcome {
        Ok(None) => None,
        Ok(Some((summary, target))) => {
            for warning in &summary.warnings {
                warn!(detail = %warning, "transfer warning");
            }
            info!(
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
                Notification::warning(text).title(format!("{verb}已取消（{cancelled_note}）"))
            } else if summary.failed > 0 {
                Notification::warning(text).title(target)
            } else {
                Notification::success(text).title(target).autohide(true)
            })
        }
        Err(error) => {
            Some(Notification::error(error.message().to_string()).title(format!("{verb}失败")))
        }
    }
}

/// 进度轮询：任务进行期间每 120ms notify 一次让进度行刷新；
/// `is_current` 返回 false（任务收尾清槽）后自动退出
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

/// 传输进行中的进度行（spinner + 单行进度 + 取消按钮）；空闲时返回 None。
/// `state_of` 让取消按钮在回调里定位面板上的状态槽
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
                    .tooltip(if cancelling {
                        "取消中…"
                    } else {
                        "取消当前导出 / 导入"
                    })
                    .disabled(cancelling)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        state_of(view).request_cancel();
                        cx.notify();
                    })),
            )
            .into_any_element(),
    )
}

/// 导入冲突策略选择对话框：显式按钮（跳过 / 合并 / 覆盖 / 停止），选择即开始导入。
/// `offer_merge` 控制是否提供条目级合并（Redis 语义不支持时隐藏）；
/// 覆盖按钮红色标示破坏性；关闭对话框 = 放弃导入
pub fn open_import_options_dialog(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    offer_merge: bool,
    on_pick: impl FnOnce(ConflictPolicy, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = title.into();
    let description: SharedString = description.into();
    let on_pick_cell = Rc::new(RefCell::new(Some(on_pick)));

    window.open_dialog(cx, move |dialog, _, _| {
        let desc = description.clone();
        let pick_cell = on_pick_cell.clone();

        let cancel_btn = crate::clickable_button("ramag-import-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| {
                window.close_dialog(app);
            });

        dialog
            .title(crate::closable_dialog_title(
                "ramag-import-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(160.0))
            // content 闭包可重入：按钮每次在闭包内构造
            .content(move |content, _, cx| {
                let muted_fg = cx.theme().muted_foreground;
                let policy_button = |id: &'static str,
                                     label: &'static str,
                                     hint: &'static str,
                                     policy: ConflictPolicy,
                                     danger: bool| {
                    let cell = pick_cell.clone();
                    let mut button = crate::clickable_button(id)
                        .small()
                        .label(label)
                        .tooltip(hint);
                    button = if danger {
                        button.danger()
                    } else if policy == ConflictPolicy::Skip {
                        button.primary()
                    } else {
                        button.outline()
                    };
                    button.on_click(move |_: &ClickEvent, window, app| {
                        window.close_dialog(app);
                        if let Some(pick) = cell.borrow_mut().take() {
                            pick(policy, window, app);
                        }
                    })
                };
                let merge_button = offer_merge.then(|| {
                    policy_button(
                        "ramag-import-merge",
                        "合并补齐（条目级去重）",
                        "保留已存在对象，重复条目跳过、缺失条目补齐（行级断点续传）",
                        ConflictPolicy::Merge,
                        false,
                    )
                });
                content.child(
                    v_flex()
                        .gap(px(10.0))
                        .child(
                            div()
                                .py(px(2.0))
                                .text_sm()
                                .text_color(muted_fg)
                                .child(desc.clone()),
                        )
                        .child(policy_button(
                            "ramag-import-skip",
                            "跳过已存在（推荐）",
                            "同名对象已存在时跳过，其余照常导入",
                            ConflictPolicy::Skip,
                            false,
                        ))
                        .children(merge_button)
                        .child(policy_button(
                            "ramag-import-overwrite",
                            "覆盖已存在（先删除，不可恢复）",
                            "同名对象先删除再导入，原数据不可恢复",
                            ConflictPolicy::Overwrite,
                            true,
                        ))
                        .child(policy_button(
                            "ramag-import-fail",
                            "遇到冲突即停止",
                            "碰到第一个同名对象立即终止导入",
                            ConflictPolicy::Fail,
                            false,
                        )),
                )
            })
            .footer(
                gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .child(cancel_btn),
            )
    });
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
