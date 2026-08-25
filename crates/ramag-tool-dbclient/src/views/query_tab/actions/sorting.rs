use gpui::Context;
use gpui_component::notification::Notification;

use super::super::QueryTab;
use super::super::paging::{PageRequest, Pager, page_sql, sort_sql};
use crate::views::result_panel::SortDir;

impl QueryTab {
    /// Re-runs a safe paginated query when the result header requests server-side sorting.
    pub(crate) fn handle_sort_changed(
        &mut self,
        previous: Option<(usize, SortDir)>,
        current: Option<(usize, SortDir)>,
        cx: &mut Context<Self>,
    ) {
        if self.running || self.transaction_busy {
            self.result
                .update(cx, |result, cx| result.restore_sort(previous, cx));
            return;
        }
        let Some(pager) = self.pager.as_ref().cloned() else {
            // Without server pagination, the existing local result sort remains usable offline.
            return;
        };
        let Some(conn) = self.connection.clone() else {
            self.result
                .update(cx, |result, cx| result.restore_sort(previous, cx));
            return;
        };
        let sort_base_sql = pager.sort_base_sql.clone();
        let sorted_base_sql = match current {
            Some((column_index, direction)) => {
                match sort_sql(&sort_base_sql, column_index, direction, conn.driver) {
                    Ok(sql) => sql,
                    Err(message) => {
                        self.result
                            .update(cx, |result, cx| result.restore_sort(previous, cx));
                        self.pending_notification =
                            Some(Notification::error(message).autohide(true));
                        cx.notify();
                        return;
                    }
                }
            }
            None => sort_base_sql.clone(),
        };
        let effective_sql = match page_sql(&sorted_base_sql, pager.page_size, 0) {
            Ok(sql) => sql,
            Err(message) => {
                self.result
                    .update(cx, |result, cx| result.restore_sort(previous, cx));
                self.pending_notification = Some(Notification::error(message).autohide(true));
                cx.notify();
                return;
            }
        };
        self.pager = Some(Pager {
            base_sql: sorted_base_sql,
            sort_base_sql: sort_base_sql.clone(),
            page: 0,
            has_more: false,
            page_size: pager.page_size,
            total: pager.total,
        });
        let title_sql = self.result.read(cx).source_sql().unwrap_or(sort_base_sql);
        self.execute_query(
            conn,
            effective_sql,
            title_sql,
            false,
            Some(PageRequest {
                page: 0,
                page_size: pager.page_size,
            }),
            self.result.clone(),
            None,
            cx,
        );
    }
}
