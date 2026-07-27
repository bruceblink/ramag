//! 工作区底部有界传输任务列表。

use gpui::{
    ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _, button::ButtonVariants as _, h_flex, v_flex,
};
use ramag_domain::entities::{OverwritePolicy, TransferDirection, TransferStatus, TransferTask};

use super::SshView;

impl SshView {
    pub(super) fn render_transfer_queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(profile_id) = self.active_workspace_id.as_ref() else {
            return div().h_0().into_any_element();
        };
        let tasks = self
            .service
            .transfer_tasks()
            .into_iter()
            .filter(|task| &task.profile_id == profile_id)
            .rev()
            .collect::<Vec<_>>();
        let border = cx.theme().border;
        let running = tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .count();
        let mut rows = v_flex().w_full();
        for task in &tasks {
            rows = rows.child(transfer_row(task.clone(), cx));
        }
        let body = if tasks.is_empty() {
            div()
                .w_full()
                .h(px(78.0))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("暂无传输任务")
                .into_any_element()
        } else {
            div()
                .id("ssh-transfer-scroll")
                .w_full()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(rows)
                .into_any_element()
        };
        v_flex()
            .w_full()
            .h(px(160.0))
            .flex_none()
            .border_t_1()
            .border_color(border)
            .child(
                h_flex()
                    .w_full()
                    .h(px(36.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(format!(
                                "传输队列（进行中 {running} / 历史 {}）",
                                tasks.len()
                            )),
                    )
                    .child(
                        ramag_ui::clickable_button("clear-ssh-transfers")
                            .ghost()
                            .xsmall()
                            .label("清除已完成")
                            .disabled(!tasks.iter().any(|task| task.status.is_terminal()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.clear_finished_transfers(cx);
                            })),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

fn transfer_row(task: TransferTask, cx: &mut Context<SshView>) -> impl IntoElement {
    let status = transfer_status(task.status);
    let status_color = match task.status {
        TransferStatus::Completed => cx.theme().success,
        TransferStatus::Failed => cx.theme().danger,
        TransferStatus::Cancelled => cx.theme().muted_foreground,
        TransferStatus::Waiting | TransferStatus::Running => cx.theme().accent,
    };
    let direction = match task.direction {
        TransferDirection::Upload => "上传",
        TransferDirection::Download => "下载",
    };
    let progress = if task.total_bytes == 0 {
        if task.transferred_bytes == 0 {
            "—".to_string()
        } else {
            format!("{} bytes", task.transferred_bytes)
        }
    } else {
        format!(
            "{:.0}% ({}/{})",
            task.transferred_bytes as f64 * 100.0 / task.total_bytes as f64,
            task.transferred_bytes,
            task.total_bytes
        )
    };
    let id = task.id.clone();
    let id_for_overwrite = id.clone();
    let id_for_cancel = id.clone();
    h_flex()
        .w_full()
        .min_h(px(44.0))
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .w(px(54.0))
                .text_xs()
                .text_color(status_color)
                .child(status),
        )
        .child(div().w(px(36.0)).text_xs().child(direction))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .text_xs()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(format!("{}  ↔  {}", task.local_path, task.remote_path)),
                )
                .when_some(task.error.clone(), |row, error| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(error),
                    )
                }),
        )
        .child(
            div()
                .w(px(150.0))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(progress),
        )
        .when(!task.status.is_terminal(), |row| {
            row.child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "cancel-ssh-transfer-{id_for_cancel}"
                )))
                .danger()
                .xsmall()
                .label("取消")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.cancel_transfer(id_for_cancel.clone(), cx);
                })),
            )
        })
        .when(
            matches!(
                task.status,
                TransferStatus::Failed | TransferStatus::Cancelled
            ),
            |row| {
                row.child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "retry-ssh-transfer-{id}"
                    )))
                    .outline()
                    .xsmall()
                    .label("重试")
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.retry_transfer(id.clone(), OverwritePolicy::Refuse, cx);
                        },
                    )),
                )
                .child(
                    ramag_ui::clickable_button(SharedString::from(format!(
                        "overwrite-retry-ssh-transfer-{id_for_overwrite}"
                    )))
                    .danger()
                    .xsmall()
                    .label("覆盖重试")
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.confirm_overwrite_retry(id_for_overwrite.clone(), window, cx);
                        },
                    )),
                )
            },
        )
}

fn transfer_status(status: TransferStatus) -> &'static str {
    match status {
        TransferStatus::Waiting => "等待",
        TransferStatus::Running => "进行中",
        TransferStatus::Completed => "完成",
        TransferStatus::Failed => "失败",
        TransferStatus::Cancelled => "已取消",
    }
}
