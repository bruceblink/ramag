use gpui::Context;
use gpui_component::notification::Notification;

use super::{QueryTab, QueryTabEvent};
use crate::sql_completion::{parse_table_reference, table_reference_at_cursor};

impl QueryTab {
    /// Emits the selected or cursor-adjacent table for navigation in the tree.
    pub(crate) fn request_table_navigation(&mut self, cx: &mut Context<Self>) {
        if self.connection.is_none() {
            self.pending_notification = Some(Notification::warning("尚未选择连接").autohide(true));
            cx.notify();
            return;
        }

        let sql = self.current_sql(cx);
        let (selected, cursor) = {
            let editor = self.editor.read(cx);
            (editor.selected_value(), editor.cursor())
        };
        let target = if selected.trim().is_empty() {
            table_reference_at_cursor(
                &sql,
                cursor,
                self.connection.as_ref().map(|connection| connection.driver),
            )
            .or_else(|| {
                self.pinned_target
                    .clone()
                    .map(|(schema, table)| (Some(schema), table))
            })
        } else {
            parse_table_reference(&selected)
        };

        let Some((schema, table)) = target else {
            self.pending_notification = Some(
                Notification::warning("请选中表引用，或将光标放在 FROM / JOIN 后的表名上")
                    .autohide(true),
            );
            cx.notify();
            return;
        };
        let Some(schema) = schema
            .or_else(|| self.active_schema.clone())
            .filter(|schema| !schema.is_empty())
        else {
            self.pending_notification = Some(
                Notification::warning("无法确定表所在 Schema，请先选择 Schema").autohide(true),
            );
            cx.notify();
            return;
        };

        cx.emit(QueryTabEvent::LocateTableRequested { schema, table });
    }
}
