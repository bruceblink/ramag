//! SQL 查询历史弹框与“填入/重跑”事件接线。

use gpui::{
    AppContext as _, Context, InteractiveElement as _, ParentElement, SharedString, Styled as _,
    Window, div, px,
};
use gpui_component::WindowExt as _;

use super::QueryPanel;
use crate::views::history_dialog::{HistoryEvent, HistoryList};

fn responsive_dialog_width(window: &Window, preferred: f32) -> gpui::Pixels {
    let available = f32::from(window.viewport_size().width);
    px((available - 32.0).max(160.0).min(preferred))
}

impl QueryPanel {
    /// 打开查询历史弹框：搜索 / 复制 / 填入 / 重跑 / 删除 / 清空。
    pub(super) fn open_history_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.connection.clone() else {
            return;
        };
        let list = cx.new(|cx| HistoryList::new(self.service.clone(), conn.id.clone(), window, cx));
        // 先关弹框再聚焦编辑器，避免焦点恢复覆盖编辑器焦点。
        self.history_sub = Some(cx.subscribe_in(
            &list,
            window,
            |this: &mut Self, _, event: &HistoryEvent, window, cx| {
                this.history_sub = None;
                match event {
                    HistoryEvent::FillEditor(sql) => {
                        window.close_dialog(cx);
                        this.fill_active_sql(sql.clone(), window, cx);
                    }
                    HistoryEvent::RunSql(sql) => {
                        window.close_dialog(cx);
                        this.fill_active_sql(sql.clone(), window, cx);
                        if let Some(tab) = this.tabs.get(this.active) {
                            tab.update(cx, |tab, cx| tab.run(window, cx));
                        }
                    }
                }
            },
        ));

        let title = SharedString::from(format!("查询历史 · {}", conn.name));
        let panel_for_close = cx.entity().clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let list = list.clone();
            let panel_for_close = panel_for_close.clone();
            let panel_for_title_close = panel_for_close.clone();
            let dialog_title = div()
                .id("sql-history-dialog-title")
                .debug_selector(|| "sql-history-dialog-title".into())
                .w_full()
                .child(ramag_ui::closable_dialog_title(
                    "sql-history-dialog-close",
                    title.clone(),
                    move |_, app| {
                        panel_for_title_close.update(app, |this, _| this.history_sub = None);
                    },
                ));
            dialog
                .title(dialog_title)
                .close_button(false)
                .on_close(move |_, _, app| {
                    panel_for_close.update(app, |this, _| this.history_sub = None);
                })
                .width(responsive_dialog_width(window, 760.0))
                .content(move |content, _, _| content.child(list.clone()))
        });
    }
}
