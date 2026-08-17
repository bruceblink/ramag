//! 数据同步全屏阻塞层。

mod result;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    ClickEvent, Context, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, Styled,
    Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    spinner::Spinner, v_flex,
};
use ramag_app::{DataSyncGate, DataSyncGatePhase, DataSyncGateSnapshot};
use ramag_domain::entities::{DataSyncStage, DataSyncTaskId, format_bytes};

use result::{format_count, format_elapsed_ms, render_result_summary};

pub struct DataSyncOverlay {
    gate: Arc<DataSyncGate>,
    cancel_confirmation: bool,
    focused_task: Option<DataSyncTaskId>,
    focus_handle: FocusHandle,
}

impl DataSyncOverlay {
    pub fn new(gate: Arc<DataSyncGate>, cx: &mut Context<Self>) -> Self {
        let gate_for_ticker = gate.clone();
        cx.spawn(async move |this, cx| {
            let mut previous = None;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let snapshot = gate_for_ticker.snapshot();
                let signature = snapshot.as_ref().map(|snapshot| {
                    (
                        snapshot.task_id.clone(),
                        snapshot.phase,
                        snapshot.progress.clone(),
                    )
                });
                if signature == previous {
                    continue;
                }
                previous = signature;
                if this
                    .update(cx, |this, cx| {
                        if gate_for_ticker.snapshot().is_none() {
                            this.cancel_confirmation = false;
                            this.focused_task = None;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            gate,
            cancel_confirmation: false,
            focused_task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn render_card(
        &mut self,
        snapshot: DataSyncGateSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let terminal = snapshot.phase.terminal();
        let cancelling = snapshot.phase == DataSyncGatePhase::Cancelling;
        let progress = &snapshot.progress;
        let title = match snapshot.phase {
            DataSyncGatePhase::Running => "正在同步数据",
            DataSyncGatePhase::Cancelling => "正在安全停止",
            DataSyncGatePhase::Completed => "数据同步完成",
            DataSyncGatePhase::Failed => "数据同步失败",
            DataSyncGatePhase::Cancelled => "数据同步已取消",
        };
        let stage = match progress.stage {
            DataSyncStage::Preparing => "准备",
            DataSyncStage::VerifyingTarget => "复核目标",
            DataSyncStage::CreatingStructure => "创建结构",
            DataSyncStage::Scanning => "扫描源数据",
            DataSyncStage::Writing => "写入缺失数据",
            DataSyncStage::Finalizing => "恢复结构与收尾",
            DataSyncStage::Cancelling => "等待安全停止点",
        };
        let theme = cx.theme();
        let (background, border, muted, foreground, success, warning, danger) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.foreground,
            theme.success,
            theme.warning,
            theme.danger,
        );
        let context = format!(
            "{} / {}  →  {} / {}",
            snapshot.context.source_connection,
            snapshot.context.source_scope,
            snapshot.context.target_connection,
            snapshot.context.target_scope
        );
        let object_progress = progress.objects_total.map_or_else(
            || progress.objects_done.to_string(),
            |total| format!("{} / {total}", progress.objects_done),
        );
        let counts = format!(
            "对象 {object_progress}  ·  扫描 {}  ·  新增 {}  ·  跳过 {}  ·  失败 {}",
            format_count(progress.scanned),
            format_count(progress.inserted),
            format_count(progress.skipped),
            format_count(progress.failed)
        );
        let details = format!(
            "传输 {}  ·  警告 {}  ·  已用时 {}",
            format_bytes(progress.bytes),
            format_count(progress.warnings),
            format_elapsed_ms(progress.elapsed_ms)
        );
        let summary = snapshot.summary.clone();
        let summary_missing = terminal && summary.is_none();
        let objects_total = progress.objects_total;
        let task_for_cancel = snapshot.task_id.clone();
        let task_for_ack = snapshot.task_id.clone();
        let mut shade = gpui::black();
        shade.a = 0.58;
        let mut danger_bg = danger;
        danger_bg.a = 0.10;

        div()
            .id("data-sync-overlay")
            .debug_selector(|| "data-sync-overlay".into())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(shade)
            .occlude()
            .track_focus(&self.focus_handle)
            .key_context("DataSyncBlocking")
            .on_key_down(
                cx.listener(|_this, _event: &KeyDownEvent, _window, cx| cx.stop_propagation()),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .id("data-sync-card")
                    .debug_selector(|| "data-sync-card".into())
                    .w(px(620.0))
                    .max_w_full()
                    .max_h_full()
                    .overflow_y_scroll()
                    .gap(px(14.0))
                    .p(px(24.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .text_color(foreground)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(10.0))
                            .when(!terminal, |row| row.child(Spinner::new().small()))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            ),
                    )
                    .child(div().text_sm().text_color(muted).child(context))
                    .when(!terminal, |card| {
                        card.child(
                            v_flex()
                                .id("sync-running-progress")
                                .debug_selector(|| "sync-running-progress".into())
                                .gap(px(8.0))
                                .child(
                                    h_flex().text_sm().child(format!("阶段：{stage}")).when(
                                        !progress.object.is_empty(),
                                        |line| {
                                            line.child(format!("  ·  当前：{}", progress.object))
                                        },
                                    ),
                                )
                                .child(div().text_sm().child(counts))
                                .child(div().text_sm().text_color(muted).child(details)),
                        )
                    })
                    .when_some(summary, |card, summary| {
                        card.child(render_result_summary(
                            &summary,
                            objects_total,
                            border,
                            muted,
                            foreground,
                            success,
                            warning,
                            danger,
                        ))
                    })
                    .when(summary_missing, |card| {
                        card.child(
                            div()
                                .text_sm()
                                .text_color(danger)
                                .child("同步已结束，但结果摘要不可用"),
                        )
                    })
                    .when_some(snapshot.error.clone(), |card, error| {
                        card.child(
                            div()
                                .id("sync-error-scroll")
                                .max_h(px(160.0))
                                .overflow_y_scroll()
                                .p(px(10.0))
                                .rounded(px(6.0))
                                .bg(danger_bg)
                                .text_sm()
                                .text_color(danger)
                                .child(error),
                        )
                    })
                    .when(self.cancel_confirmation && !terminal, |card| {
                        card.child(
                            v_flex()
                                .gap(px(8.0))
                                .p(px(10.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(danger)
                                .child(
                                    div()
                                        .text_sm()
                                        .child("取消会等待当前批次或结构创建安全收尾。"),
                                )
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap(px(8.0))
                                        .child(
                                            crate::clickable_button("sync-cancel-back")
                                                .ghost()
                                                .small()
                                                .label("继续同步")
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _, cx| {
                                                        this.cancel_confirmation = false;
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .child(
                                            div()
                                                .debug_selector(|| "sync-cancel-confirm".into())
                                                .child(
                                                    crate::clickable_button("sync-cancel-confirm")
                                                        .danger()
                                                        .small()
                                                        .label("取消")
                                                        .on_click(cx.listener(
                                                            move |this, _: &ClickEvent, _, cx| {
                                                                this.gate.request_cancel_current(
                                                                    &task_for_cancel,
                                                                );
                                                                this.cancel_confirmation = false;
                                                                cx.notify();
                                                            },
                                                        )),
                                                ),
                                        ),
                                ),
                        )
                    })
                    .when(!terminal && !self.cancel_confirmation, |card| {
                        card.child(
                            h_flex().justify_end().child(
                                div().debug_selector(|| "sync-cancel".into()).child(
                                    crate::clickable_button("sync-cancel")
                                        .danger()
                                        .small()
                                        .icon(IconName::Close)
                                        .label(if cancelling {
                                            "正在取消…"
                                        } else {
                                            "取消"
                                        })
                                        .disabled(cancelling)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.cancel_confirmation = true;
                                            cx.notify();
                                        })),
                                ),
                            ),
                        )
                    })
                    .when(terminal, |card| {
                        card.child(
                            h_flex().justify_end().child(
                                div().debug_selector(|| "sync-result-ack".into()).child(
                                    crate::clickable_button("sync-result-ack")
                                        .primary()
                                        .small()
                                        .label("关闭")
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, _, cx| {
                                                this.gate.acknowledge_current(&task_for_ack);
                                                this.cancel_confirmation = false;
                                                cx.notify();
                                            },
                                        )),
                                ),
                            ),
                        )
                    }),
            )
    }
}

impl Render for DataSyncOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.gate.snapshot();
        let active = snapshot.is_some();
        if let Some(snapshot) = snapshot.as_ref()
            && self.focused_task.as_ref() != Some(&snapshot.task_id)
        {
            self.focused_task = Some(snapshot.task_id.clone());
            window.focus(&self.focus_handle, cx);
        }
        div()
            .when(active, |root| root.absolute().inset_0())
            .when_some(snapshot, |root, snapshot| {
                root.child(self.render_card(snapshot, cx))
            })
    }
}

#[cfg(test)]
#[path = "data_sync_overlay_tests.rs"]
mod tests;
