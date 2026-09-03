use gpui::{Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use ramag_domain::entities::MAX_SQL_QUERY_BYTES;

use super::{QueryTab, QueryTabEvent};
use crate::views::result_panel::{ResultPanel, ResultPanelEvent};

pub(super) fn subscribe(
    editor: &Entity<InputState>,
    result: &Entity<ResultPanel>,
    plan_result: &Entity<ResultPanel>,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) -> (gpui::Subscription, gpui::Subscription, gpui::Subscription) {
    let editor_for_sub = editor.clone();
    let editor_sub = cx.subscribe_in(
        editor,
        window,
        move |this: &mut QueryTab, _, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            this.clear_pager(cx);
            if ramag_ui::clamp_multiline_input_value(
                &editor_for_sub,
                MAX_SQL_QUERY_BYTES,
                window,
                cx,
            ) {
                this.pending_notification = Some(
                    Notification::warning(format!(
                        "SQL 编辑器最多保留 {} MiB，超出部分已截断",
                        MAX_SQL_QUERY_BYTES / 1024 / 1024
                    ))
                    .autohide(true),
                );
            }
            if this.pinned_target.is_some() && this.has_user_draft(cx) {
                this.pinned_target = None;
                this.result.update(cx, |result, cx| {
                    result.clear_pending_cell_edits(cx);
                    result.clear_editable_target(cx);
                });
            }
            this.invalidate_query_context(cx);
            this.schedule_column_prefetch(cx);
            cx.emit(QueryTabEvent::DraftChanged);
        },
    );
    let result_sub = cx.subscribe_in(
        result,
        window,
        |this: &mut QueryTab, _, event: &ResultPanelEvent, window, cx| match event {
            ResultPanelEvent::PageRequested(page) => this.handle_page(*page, cx),
            ResultPanelEvent::PageSizeChanged(page_size) => this.handle_page_size(*page_size, cx),
            ResultPanelEvent::SortChanged { previous, current } => {
                this.handle_sort_changed(*previous, *current, cx)
            }
            ResultPanelEvent::RowSearchChanged => cx.notify(),
            ResultPanelEvent::RowFilterApply => this.handle_row_filter_apply(window, cx),
            ResultPanelEvent::MutationCompleted => this.mark_transaction_dirty(cx),
            ResultPanelEvent::MutationFailed(message) => {
                this.mark_transaction_error(message.clone(), cx)
            }
            ResultPanelEvent::Retry => this.handle_run(window, cx),
        },
    );
    let plan_result_sub = cx.subscribe_in(
        plan_result,
        window,
        |this: &mut QueryTab, _, event: &ResultPanelEvent, window, cx| {
            if matches!(event, ResultPanelEvent::Retry) {
                this.handle_explain(window, cx);
            }
        },
    );

    (editor_sub, result_sub, plan_result_sub)
}
