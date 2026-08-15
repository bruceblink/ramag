//! 对象存储工作区的悬浮传输任务列表。

use std::sync::atomic::Ordering;

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px, relative,
};
use gpui_component::{
    ActiveTheme, Disableable as _, IconName, Sizable as _, button::ButtonVariants as _, h_flex,
    v_flex,
};
use ramag_domain::entities::format_bytes;

use super::model::{ObjectStorageView, ObjectTransferStatus, TransferHistoryUi, TransferUi};

impl ObjectStorageView {
    pub(super) fn render_transfer_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.transfers_visible {
            return div().h_0().into_any_element();
        }
        let border = cx.theme().border;
        let completed = self
            .transfer_history
            .iter()
            .filter(|record| record.status == ObjectTransferStatus::Completed)
            .count();
        let total = self.transfers.len() + self.transfer_history.len();
        let mut rows = v_flex().w_full();
        if total == 0 {
            rows = rows.child(
                h_flex()
                    .h(px(72.0))
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("暂无传输任务"),
            );
        } else {
            for transfer in &self.transfers {
                rows = rows.child(active_transfer_row(transfer, cx));
            }
            for (index, record) in self.transfer_history.iter().enumerate() {
                rows = rows.child(history_transfer_row(index, record, cx));
            }
        }

        v_flex()
            .id("object-transfer-panel")
            .debug_selector(|| "object-transfer-panel".into())
            .absolute()
            .top(px(12.0))
            .right(px(12.0))
            .w(px(520.0))
            .max_w(relative(0.8))
            .max_h(px(360.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(border)
            .bg(cx.theme().background)
            .shadow_lg()
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
                            .child(format!("完成 {completed}/{total}")),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                ramag_ui::clickable_button("clear-object-transfer-history")
                                    .ghost()
                                    .xsmall()
                                    .label("清理")
                                    .disabled(self.transfer_history.is_empty())
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.transfer_history.clear();
                                        if this.transfers.is_empty() {
                                            this.transfers_visible = false;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                ramag_ui::clickable_button("hide-object-transfers")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip("关闭")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.transfers_visible = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("object-transfer-scroll")
                    .w_full()
                    .max_h(px(324.0))
                    .overflow_y_scroll()
                    .child(rows),
            )
            .into_any_element()
    }
}

fn active_transfer_row(
    transfer: &TransferUi,
    cx: &mut Context<ObjectStorageView>,
) -> impl IntoElement {
    let transfer_id = transfer.id;
    let transferred = transfer.transferred.load(Ordering::Relaxed);
    let total = transfer.total.load(Ordering::Relaxed);
    let cancelling = transfer.cancellation.is_cancelled();
    let status = if cancelling {
        "取消中"
    } else if transferred == 0 && total == 0 {
        "等待"
    } else {
        "传输中"
    };
    let status_color = if cancelling {
        cx.theme().muted_foreground
    } else {
        cx.theme().accent
    };

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
        .child(
            div()
                .w(px(36.0))
                .text_xs()
                .child(transfer.direction.label()),
        )
        .child(transfer_paths(
            transfer.local_path.clone(),
            transfer.label.clone(),
            None,
            cx,
        ))
        .child(
            div()
                .w(px(150.0))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(transfer_progress(transferred, total)),
        )
        .when(!cancelling, |row| {
            row.child(
                ramag_ui::clickable_button(SharedString::from(format!(
                    "cancel-object-transfer-{transfer_id}"
                )))
                .danger()
                .xsmall()
                .label("取消")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(transfer) = this
                        .transfers
                        .iter()
                        .find(|transfer| transfer.id == transfer_id)
                    {
                        transfer.cancellation.cancel();
                        cx.notify();
                    }
                })),
            )
        })
}

fn history_transfer_row(
    index: usize,
    record: &TransferHistoryUi,
    cx: &Context<ObjectStorageView>,
) -> impl IntoElement {
    let (status, color) = match record.status {
        ObjectTransferStatus::Completed => ("完成", cx.theme().success),
        ObjectTransferStatus::Failed => ("失败", cx.theme().danger),
        ObjectTransferStatus::Cancelled => ("取消", cx.theme().muted_foreground),
    };
    h_flex()
        .id(("object-transfer-history", index))
        .w_full()
        .min_h(px(44.0))
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().w(px(54.0)).text_xs().text_color(color).child(status))
        .child(div().w(px(36.0)).text_xs().child(record.direction.label()))
        .child(transfer_paths(
            record.local_path.clone(),
            record.label.clone(),
            record.error.clone(),
            cx,
        ))
        .child(
            div()
                .w(px(150.0))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("—"),
        )
}

fn transfer_paths(
    local_path: String,
    object_key: String,
    error: Option<String>,
    cx: &gpui::App,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(
            div()
                .text_xs()
                .overflow_hidden()
                .text_ellipsis()
                .child(format!("{local_path}  ↔  {object_key}")),
        )
        .when_some(error, |row, error| {
            row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(error),
            )
        })
}

fn transfer_progress(transferred: u64, total: u64) -> String {
    if total == 0 {
        if transferred == 0 {
            "—".into()
        } else {
            format_bytes(transferred)
        }
    } else {
        format!(
            "{:.0}% ({}/{})",
            transferred as f64 * 100.0 / total as f64,
            format_bytes(transferred),
            format_bytes(total)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::transfer_progress;

    #[test]
    fn transfer_progress_matches_ssh_style() {
        assert_eq!(transfer_progress(0, 0), "—");
        assert_eq!(transfer_progress(512, 1024), "50% (512 B/1 KiB)");
    }
}
