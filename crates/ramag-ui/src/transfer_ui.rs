//! 按库导出 / 导入的共享 UI 件：传输状态槽（取消位 + 进度快照）、
//! 进度轮询 ticker、导入冲突策略选择对话框。三个数据库工具共用

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
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
                    .disabled(cancelling)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        state_of(view).request_cancel();
                        cx.notify();
                    })),
            )
            .into_any_element(),
    )
}

/// 导入确认回调：冲突策略 + 已选文件列表
type ImportPickHandler = Box<dyn FnOnce(ConflictPolicy, Vec<PathBuf>, &mut Window, &mut App)>;

/// 导入选项表单：冲突策略单选 + 系统框多选文件，点「导入」才回调开始。
/// 表单自持状态（对话框 content 每帧重建，无法存选择），三个数据库工具共用
struct ImportOptionsForm {
    description: SharedString,
    offer_merge: bool,
    filter_label: &'static str,
    extensions: &'static [&'static str],
    policy: ConflictPolicy,
    files: Vec<PathBuf>,
    /// 系统文件框打开期间防重入
    picking: bool,
    on_pick: Rc<RefCell<Option<ImportPickHandler>>>,
}

impl ImportOptionsForm {
    /// 打开系统多选文件框；取消选择时保留已选列表
    fn pick_files(&mut self, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        let filter_label = self.filter_label;
        let extensions = self.extensions;
        cx.spawn(async move |this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter(filter_label, extensions)
                .pick_files()
                .await;
            let _ = this.update(cx, |this, cx| {
                this.picking = false;
                if let Some(handles) = picked
                    && !handles.is_empty()
                {
                    this.files = handles
                        .iter()
                        .map(|handle| handle.path().to_path_buf())
                        .collect();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn files_summary(&self) -> String {
        fn name_of(path: &std::path::Path) -> String {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        }
        match self.files.as_slice() {
            [] => "未选择文件".to_string(),
            [only] => name_of(only),
            [first, second] => format!("{}、{}", name_of(first), name_of(second)),
            [first, ..] => format!("{} 等 {} 个文件", name_of(first), self.files.len()),
        }
    }
}

impl Render for ImportOptionsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        let entity = cx.entity();

        let policy_button = {
            let entity = entity.clone();
            move |id: &'static str,
                  label: &'static str,
                  hint: &'static str,
                  value: ConflictPolicy,
                  danger: bool,
                  selected: bool| {
                let entity = entity.clone();
                let mut button = crate::clickable_button(id)
                    .small()
                    .label(label)
                    .tooltip(hint);
                button = match (selected, danger) {
                    (true, true) => button.danger(),
                    (true, false) => button.primary(),
                    (false, _) => button.outline(),
                };
                button.on_click(move |_: &ClickEvent, _, app| {
                    entity.update(app, |this, cx| {
                        this.policy = value;
                        cx.notify();
                    });
                })
            }
        };
        let merge_button = self.offer_merge.then(|| {
            policy_button(
                "ramag-import-merge",
                "合并",
                "保留对象，补齐缺失条目",
                ConflictPolicy::Merge,
                false,
                self.policy == ConflictPolicy::Merge,
            )
        });

        let pick_button = {
            let entity = entity.clone();
            crate::clickable_button("ramag-import-pick")
                .outline()
                .small()
                .label(if self.files.is_empty() {
                    "选文件"
                } else {
                    "重选"
                })
                .disabled(self.picking)
                .on_click(move |_: &ClickEvent, _, app| {
                    entity.update(app, |this, cx| this.pick_files(cx));
                })
        };
        let confirm_button = {
            let entity = entity.clone();
            crate::clickable_button("ramag-import-confirm")
                .primary()
                .small()
                .label("导入")
                .disabled(self.files.is_empty() || self.picking)
                .on_click(move |_: &ClickEvent, window, app| {
                    let taken = entity.update(app, |this, _| {
                        if this.files.is_empty() {
                            return None;
                        }
                        this.on_pick
                            .borrow_mut()
                            .take()
                            .map(|handler| (handler, this.policy, std::mem::take(&mut this.files)))
                    });
                    if let Some((handler, policy, files)) = taken {
                        window.close_dialog(app);
                        handler(policy, files, window, app);
                    }
                })
        };
        let cancel_button = crate::clickable_button("ramag-import-cancel")
            .ghost()
            .small()
            .label("取消")
            .on_click(|_: &ClickEvent, window, app| window.close_dialog(app));

        v_flex()
            .gap(px(10.0))
            .child(
                div()
                    .py(px(2.0))
                    .text_sm()
                    .text_color(muted_fg)
                    .child(self.description.clone()),
            )
            .child(policy_button(
                "ramag-import-skip",
                "跳过",
                "跳过同名对象（推荐）",
                ConflictPolicy::Skip,
                false,
                self.policy == ConflictPolicy::Skip,
            ))
            .children(merge_button)
            .child(policy_button(
                "ramag-import-overwrite",
                "覆盖",
                "删除同名对象后导入，不可恢复",
                ConflictPolicy::Overwrite,
                true,
                self.policy == ConflictPolicy::Overwrite,
            ))
            .child(policy_button(
                "ramag-import-fail",
                "停止",
                "遇到同名对象即停止",
                ConflictPolicy::Fail,
                false,
                self.policy == ConflictPolicy::Fail,
            ))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(pick_button)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted_fg)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.files_summary()),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(cancel_button)
                    .child(confirm_button),
            )
    }
}

/// 导入选项对话框：冲突策略单选 + 文件多选，确认后回调 `(policy, files)`。
/// `offer_merge` 控制是否提供条目级合并（Redis 语义不支持时隐藏）；
/// `file_filter` 为系统文件框的（类型名, 扩展名列表）；关闭对话框 = 放弃导入
pub fn open_import_options_dialog(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    offer_merge: bool,
    file_filter: (&'static str, &'static [&'static str]),
    on_pick: impl FnOnce(ConflictPolicy, Vec<PathBuf>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = title.into();
    let description: SharedString = description.into();
    let (filter_label, extensions) = file_filter;
    let form = cx.new(|_| ImportOptionsForm {
        description,
        offer_merge,
        filter_label,
        extensions,
        policy: ConflictPolicy::Skip,
        files: Vec::new(),
        picking: false,
        on_pick: Rc::new(RefCell::new(Some(Box::new(on_pick)))),
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let form = form.clone();
        dialog
            .title(crate::closable_dialog_title(
                "ramag-import-close",
                title.clone(),
                |_, _| {},
            ))
            .close_button(false)
            .margin_top(px(160.0))
            .content(move |content, _, _| content.child(form.clone()))
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
